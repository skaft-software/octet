"""Quiet voice controls with section spacing and paged ephemeral captions.

The host owns focus and may crop above-editor widgets without reporting their
allocated height. Captions therefore ALWAYS say "preview", even on their last
page. No caption text is sent to the conversation or written to disk.
"""
from collections import deque
from functools import lru_cache
import os
import threading
import unicodedata


BARS = "▁▂▃▄▅▆▇█"
# Keep the latest user AND voice utterance (64 KiB each), plus labels.
MAX_CAPTION_BYTES = 2 * (65536 + 128)
# Leave room for SGR under the host's 16 KiB/line limit.
MAX_CONTENT_BYTES = 8192
MAX_COLUMNS = 4096


def _escape(char):
    code = ord(char)
    return (f"\\x{code:02x}" if code <= 255 else
            f"\\u{code:04x}" if code <= 65535 else f"\\U{code:08x}")


def _safe_text(text):
    # Keep line breaks and tab stops; expose rather than execute controls, bidi
    # overrides, surrogates and invisible format characters. Joiners are needed
    # by legitimate graphemes and are handled below, never emitted on their own.
    result = []
    for index, char in enumerate(text):
        if char in "\u200c\u200d" and (index == 0 or (
                unicodedata.category(text[index - 1])[0] == "C" and
                text[index - 1] not in "\u200c\u200d")):
            result.append(_escape(char))
        else:
            result.append(char if char in "\n\t\u200c\u200d" or
                          unicodedata.category(char)[0] != "C" else _escape(char))
    return "".join(result)


def _hangul(char):
    code = ord(char)
    if 0x1100 <= code <= 0x115f or 0xa960 <= code <= 0xa97c:
        return "L"
    if 0x1160 <= code <= 0x11a7 or 0xd7b0 <= code <= 0xd7c6:
        return "V"
    if 0x11a8 <= code <= 0x11ff or 0xd7cb <= code <= 0xd7fb:
        return "T"
    if 0xac00 <= code <= 0xd7a3:
        return "LV" if (code - 0xac00) % 28 == 0 else "LVT"
    return ""


def _graphemes(text):
    """Conservatively keep marks, joiners, flags and Hangul syllables together.

    No optional Unicode dependency is installed by this extension. For unusual
    or oversized clusters we use visible code-point escapes, not a broken glyph
    or a discarded suffix. Ambiguous characters use the host's narrow policy.
    """
    cluster = []
    regional = 0
    for char in text:
        code = ord(char)
        is_regional = 0x1f1e6 <= code <= 0x1f1ff
        previous = cluster[-1] if cluster else ""
        hangul = _hangul(previous) if previous else ""
        joins = bool(cluster) and char != "\t" and previous != "\t" and (
            unicodedata.category(char).startswith("M") or
            char in "\u200c\u200d" or previous in "\u200c\u200d" or
            (unicodedata.combining(previous) == 9 and unicodedata.category(char).startswith("L")) or
            (0x1f3fb <= code <= 0x1f3ff and ord(cluster[0]) >= 0x1f000) or
            (is_regional and regional == 1) or
            (hangul == "L" and _hangul(char) in ("L", "V", "LV", "LVT")) or
            (hangul in ("LV", "V") and _hangul(char) in ("V", "T")) or
            (hangul in ("LVT", "T") and _hangul(char) == "T"))
        if cluster and not joins:
            yield "".join(cluster)
            cluster = []
            regional = 0
        cluster.append(char)
        regional = regional + 1 if is_regional else 0
    if cluster:
        yield "".join(cluster)


def _emoji_base(char):
    return (0x1f000 <= ord(char) <= 0x1faff or 0x2600 <= ord(char) <= 0x27bf or
            char in "©®‼⁉™ℹ↔↕↖↗↘↙↩↪⌚⌛⌨⏏⏩⏪⏫⏬⏭⏮⏯⏰⏱⏲⏳⏸⏹⏺Ⓜ▪▫▶◀◻◼◽◾⬅⬆⬇⬛⬜⭐⭕")


def _emoji_join(cluster):
    parts = cluster.split("\u200d")
    return all(part and _emoji_base(part[0]) and all(
        unicodedata.category(char).startswith("M") or 0x1f3fb <= ord(char) <= 0x1f3ff
        for char in part[1:]) for part in parts)


def _cluster_cells(cluster):
    if all(0x1f1e6 <= ord(char) <= 0x1f1ff for char in cluster):
        return 2  # both lone regional indicators and flag pairs
    if "\u200d" in cluster and _emoji_join(cluster):
        return 2
    if ("\ufe0f" in cluster and _emoji_base(cluster[0])) or ("\u20e3" in cluster and cluster[0] in "#*0123456789"):
        return 2
    if any(0x1f3fb <= ord(char) <= 0x1f3ff for char in cluster) and _emoji_base(cluster[0]):
        return 2
    cluster = unicodedata.normalize("NFC", cluster)
    return sum(0 if unicodedata.category(char) in ("Mn", "Me") or
               char in "\u200c\u200d" else
               2 if unicodedata.east_asian_width(char) in "WF" else 1
               for char in cluster)


def cell_width(text):
    """Display width of safe, unstyled text using the host's narrow policy."""
    return sum(_cluster_cells(cluster) for cluster in _graphemes(text))


def _display_units(text, width):
    for cluster in _graphemes(text):
        # Bound zero-width storms and avoid combining with neighboring content.
        if len(cluster) > 64 or len(cluster.encode("utf-8")) > 512:
            yield from ((char, 1) for part in cluster for char in _escape(part))
            continue
        if cluster[0] in "\u200c\u200d" or ("\u200d" in cluster and not _emoji_join(cluster)):
            yield from ((char, 1) for part in cluster for char in _escape(part))
            continue
        if unicodedata.category(cluster[0]).startswith("M"):
            cluster = "◌" + cluster
        cells = _cluster_cells(cluster)
        if cells > width:
            # A two-cell glyph cannot fit a one-cell terminal. Its full, visible
            # spelling is preferable to omission or an orphaned combining mark.
            yield from ((char, 1) for part in cluster for char in _escape(part))
        else:
            yield cluster, cells


@lru_cache(maxsize=2)
def wrap_caption(text, width):
    """Return every safe caption row; never ellipsize or discard a suffix.

    Cache the combined text and user portion at the current width. Byte limits
    apply BEFORE normalization or segmentation, bounding pathological input.
    """
    if len(text) > MAX_CAPTION_BYTES or len(text.encode("utf-8", "surrogatepass")) > MAX_CAPTION_BYTES:
        raise ValueError("Voice caption exceeds its display byte budget")
    width = max(1, min(MAX_COLUMNS, width))
    lines = []
    for paragraph in _safe_text(text).split("\n"):
        units, used, size, word_break = [], 0, 0, 0
        for cluster, cells in _display_units(paragraph, width):
            expanded = " " * (4 - used % 4) if cluster == "\t" else cluster
            additions = [(" ", 1)] * len(expanded) if cluster == "\t" else [(expanded, cells)]
            for value, count in additions:
                encoded = len(value.encode("utf-8"))
                while units and (used + count > width or size + encoded > MAX_CONTENT_BYTES):
                    cut = word_break or len(units)
                    lines.append("".join(unit[0] for unit in units[:cut]))
                    units = units[cut:]
                    used = sum(unit[1] for unit in units)
                    size = sum(len(unit[0].encode("utf-8")) for unit in units)
                    word_break = 0
                units.append((value, count))
                used += count
                size += encoded
                if value.isspace():
                    word_break = len(units)
        lines.append("".join(unit[0] for unit in units))
    return tuple(lines)


def clip(text, width):
    """Cell-safe prefix for fixed chrome only; captions use lossless wrapping."""
    used, result = 0, []
    for cluster, cells in _display_units(_safe_text(" ".join(text.split())), max(1, width)):
        if used + cells > width:
            break
        result.append(cluster)
        used += cells
    return "".join(result)


def _layout(columns, rows):
    columns = max(1, min(MAX_COLUMNS, columns))
    # Budget approximately a third of the terminal, not the unknown widget
    # allocation. The optional top spacer is in addition to this budget. Prefer
    # readable spacing, but keep short terminals compact.
    budget = max(1, rows // 3)
    spaced = budget >= 7  # heading, meter, one caption and controls plus gaps
    caption_rows = min(4, max(1, budget - 6 if spaced else budget - 3))
    return columns, caption_rows, spaced


def caption_text(snapshot):
    """Keep both current utterances accessible; older services supply caption."""
    if "user_transcript" in snapshot or "voice_transcript" in snapshot:
        return "\n".join(f"{label}: {snapshot[key]}" for key, label in (
            ("user_transcript", "You"), ("voice_transcript", "Voice")) if snapshot.get(key))
    return snapshot.get("caption", "")


def caption_page_count(text, columns, rows=24):
    width, _, _ = _layout(columns, rows)
    # Windows overlap by one wrapped row: the host may show only that row.
    return max(1, len(wrap_caption(text, width)))


def render(snapshot, columns, microphone, speaker, color=True, *, rows=24, caption_page=0):
    width, caption_rows, spaced = _layout(columns, rows)

    def styled(text, code):
        return f"\x1b[{code}m{text}\x1b[0m" if color and code and text else text

    def fitting(*choices):
        return next((choice for choice in choices if cell_width(choice) <= width), "")

    muted = snapshot["muted"]
    state = snapshot["state"]
    if state in ("listening", "speaking", "muted"):
        state = "muted" if muted else ("speaking" if snapshot["speaker_level"] > 0.01 else "listening")
    elapsed = snapshot["elapsed_seconds"]
    timer = f"{elapsed // 60:02d}:{elapsed % 60:02d}"
    caption = caption_text(snapshot)
    caption_lines = wrap_caption(caption, width) if caption else ()
    pages = max(1, len(caption_lines))
    page = min(max(0, caption_page), pages - 1)
    title = f"{'◌' if muted else '●'} voice · {state}"
    if caption:
        preview = f"preview {page + 1}/{pages}"
        # The host can crop the bottom controls even while showing the heading.
        with_keys = (f"{title} · {preview} · ^F6/^F7", f"voice · {preview} · ^F6/^F7",
                     f"{preview} · ^F6/^F7") if pages > 1 else ()
        title = fitting(*with_keys, f"{title} · {preview}", f"voice · {preview}", preview, "preview", "!")
    else:
        title = fitting(title, f"voice {state}", "voice", "V")
    if cell_width(title) + len(timer) + 3 <= width:
        title += " · " + timer
    lines = [styled(title, "1;36" if not muted else "1")]

    labels_width = 17 if muted else 11  # labels and the gap, excluding bars
    count = min(24, max(1, (width - labels_width) // 2))

    def wave(history, silent=False):
        values = ([0.0] * count if silent else ([0.0] * count + list(history))[-count:])
        return "".join(BARS[min(7, int(max(0, min(1, value)) * 7))] for value in values)

    mic = ("MIC muted " if muted else "MIC ") + wave(microphone, muted)
    out = "OUT " + wave(speaker)
    if width >= labels_width + 2:
        lines.append(styled(mic, "2" if muted else "36") + "   " + styled(out, "35"))
    else:
        lines.append(fitting("MIC muted" if muted else "MIC live", "muted" if muted else "live"))
    caption_window = caption_lines[page:page + caption_rows]
    lines.extend(caption_window)
    mute = "unmute" if muted else "mute"
    controls = f"^F10 stop · ^F9 {mute} · ^F8 toggle"
    if pages > 1:
        controls = fitting(f"{controls} · ^F6/^F7 captions", "^F10 stop · ^F6/^F7 captions",
                           "^F6/^F7 captions", "^F6/^F7", "pages", "↔")
    else:
        controls = fitting(f"{controls} · headphones recommended", controls, f"^F10 stop · ^F9 {mute}",
                           "/voice stop", "^F10 stop", "^F10")
    lines.append(styled(controls, "2"))
    if spaced:
        # Separate the sections, not the wrapped transcript lines. The host may
        # crop these rows along with the rest of an above-editor widget.
        lines.insert(1, "")  # Between heading and meters.
        lines.insert(3, "")  # Before captions (or controls when no caption).
        if caption_window:
            lines.insert(-1, "")  # Between captions and controls.
    if rows >= 5:
        lines.insert(0, "")  # Separate the strip from the preceding content.
    return lines[:max(1, rows)]


class VoiceStrip:
    def __init__(self, extension, service):
        self.ext, self.service = extension, service
        self._lock = threading.RLock()
        self._stop = threading.Event()
        self._thread = None
        self._mount = None
        self._serial = 0
        self._caption = ""
        self._caption_page = 0
        self._browsing = False
        self._mic, self._speaker = deque(maxlen=24), deque(maxlen=24)
        self._color = "NO_COLOR" not in os.environ

    async def _session_epoch(self, owner, parent):
        # Check on the service loop, alongside reserve/stop. A superseded start
        # must not attach its old surface to a newer voice reservation.
        if self.service._owner == owner and self.service._parent == parent:
            return self.service._epoch
        return None

    async def _stop_mount(self, mount):
        # The mount may have been detached before this coroutine gets scheduled.
        # Check the service generation on its loop immediately before stop so a
        # delayed retirement or failed frame cannot stop a newer session.
        with self._lock:
            if self._serial != mount["serial"]:
                return False
        if (self.service._epoch != mount["epoch"] or
                self.service._owner != mount["resource_owner"] or
                self.service._parent != mount["parent_request_id"]):
            return False
        await self.service.stop()
        return True

    def open(self, owner, parent):
        epoch = self.service.call(self._session_epoch(owner, parent))
        with self._lock:
            if epoch is None or epoch != self.service._epoch or self._stop.is_set():
                raise ValueError("Voice session was replaced before opening its strip")
            old = self._detach_locked()
            self._serial += 1
            surface = f"voice-{self._serial}"
            mount = dict(surface_id=surface, resource_owner=dict(owner), parent_request_id=parent,
                         serial=self._serial, epoch=epoch, columns=None, rows=None, revision=0,
                         opened=False, host_closed=False)
            # Install the pending identity before any protocol write, so even an
            # in-flight open/old close cannot keep retirement from stopping audio.
            self._mount = mount
            self._mic.clear()
            self._speaker.clear()
        self._close_mount(old)
        with self._lock:
            if self._mount is not mount or self.service._epoch != epoch or self._stop.is_set():
                if self._mount is mount:
                    self._detach_locked()
                raise ValueError("Voice session was closed while opening its strip")
        try:
            geometry = self.ext.request("ui/open", {"surface_id": surface, "title": "Voice",
                "placement": "above_editor", "resource_owner": owner}, parent_request_id=parent, timeout=3)
        except BaseException:
            with self._lock:
                if self._mount is mount:
                    self._detach_locked()
            raise
        with self._lock:
            mount["opened"] = True
            if self._mount is mount and self.service._epoch == epoch and not self._stop.is_set():
                # A resize can arrive while ui/open is in flight.
                if mount["columns"] is None:
                    mount.update(columns=geometry["columns"], rows=geometry["rows"])
                if self._thread is None:
                    self._thread = threading.Thread(target=self._run, name="octet-voice-ui", daemon=True)
                    self._thread.start()
                return
            if self._mount is mount:
                self._detach_locked()
        # A successful late open needs one close; an observed host retirement
        # already removed it. Never retry an ambiguous open or close.
        self._close_mount(mount)
        raise ValueError("Voice session was closed while opening its strip")

    def event(self, event):
        with self._lock:
            mount = self._mount
            if mount is None or event.get("surface_id") != mount["surface_id"]:
                return
            if "columns" in event:
                mount.update(columns=event["columns"], rows=event["rows"])
                return
            mount["host_closed"] = True
            self._detach_locked()
        # No protocol write or display lock can delay owner retirement/rescue.
        self.service.call(self._stop_mount(mount), timeout=6)

    def page(self, delta):
        """Scroll the current caption by wrapped rows without taking focus.

        Integrators bind negative/positive deltas to previous/next commands or
        shortcuts. Bounds are clamped, not wrapped. Long live captions follow
        the most recent speaker until the user scrolls; updates then retain the
        chosen row. Pressing down again at the last row resumes following.
        Resize reflows the full text. Windows overlap so every row reaches the top even if the host crops the
        rest. The heading's preview position counts rows, not disjoint pages.
        Returns False when there is no live strip/caption to browse.
        """
        with self._lock:
            if self._mount is None or not self._caption:
                return False
            pages = caption_page_count(self._caption, self._mount["columns"], self._mount["rows"])
            previous = self._caption_page
            self._caption_page = min(max(0, previous + delta), pages - 1)
            if delta > 0 and previous == pages - 1:
                self._browsing = False  # A second down at the end resumes follow.
            elif self._caption_page != previous:
                self._browsing = True
            return True

    def _detach_locked(self):
        mount, self._mount = self._mount, None
        self._caption, self._caption_page, self._browsing = "", 0, False
        wrap_caption.cache_clear()
        return mount

    def _close_mount(self, mount):
        if mount is not None and mount["opened"] and not mount["host_closed"]:
            try:
                self.ext.request("ui/close", {key: mount[key] for key in (
                    "surface_id", "resource_owner", "parent_request_id")}, timeout=2)
            except Exception:
                # Owner retirement already removes the surface; never retry it.
                pass

    def _run(self):
        while not self._stop.wait(0.1):
            mount = None
            try:
                with self._lock:
                    mount = self._mount
                    if mount is None or not mount["opened"]:
                        continue
                    snapshot = self.service.snapshot()
                    if (self.service._epoch != mount["epoch"] or
                            snapshot["state"] in ("stopped", "failed")):
                        self._detach_locked()
                        frame = None
                    else:
                        self._mic.append(snapshot["microphone_level"])
                        self._speaker.append(snapshot["speaker_level"])
                        mount["revision"] += 1
                        frame = {key: mount[key] for key in (
                            "surface_id", "resource_owner", "columns", "rows", "revision")}
                        caption = caption_text(snapshot)
                        self._caption = caption
                        width, visible_rows, _ = _layout(mount["columns"], mount["rows"])
                        user = snapshot.get("user_transcript", "")
                        # Wrap the user portion first, preserving both cache entries
                        # while a separate live voice response keeps changing.
                        user_rows = len(wrap_caption(f"You: {user}", width)) if user else 0
                        pages = caption_page_count(caption, mount["columns"], mount["rows"])
                        if not self._browsing:
                            if pages <= visible_rows:
                                self._caption_page = 0  # Show both speakers when they fit.
                            elif snapshot.get("caption", "").startswith("Voice: "):
                                self._caption_page = max(user_rows, pages - visible_rows)
                            elif user:
                                self._caption_page = max(0, user_rows - visible_rows)
                            else:
                                self._caption_page = 0  # Legacy caption snapshots.
                        self._caption_page = min(self._caption_page, pages - 1)
                        frame["lines"] = render(snapshot, mount["columns"], self._mic, self._speaker, self._color,
                                                rows=mount["rows"], caption_page=self._caption_page)
                # Frames and reverse RPCs share a potentially stalled writer.
                # Never hold the retirement lock across either kind of write.
                if frame is None:
                    self._close_mount(mount)
                else:
                    self.ext.publish_ui_frame(frame)
            except Exception:
                with self._lock:
                    if self._mount is not mount:
                        continue  # A late failure belongs to a retired/replaced mount.
                    self._detach_locked()
                try:
                    stopped = self.service.call(self._stop_mount(mount), timeout=6)
                except Exception:
                    stopped = False  # Supervision is the final cleanup fence.
                # Release audio before attempting any further protocol writes.
                self._close_mount(mount)
                if stopped:
                    try:
                        self.ext.notify("Voice display failed; voice stopped. Check /voice status.", level="error")
                    except Exception:
                        pass

    def close(self):
        self._stop.set()
        with self._lock:
            mount = self._detach_locked()
        if mount is not None:
            try:
                self.service.call(self._stop_mount(mount), timeout=6)
            except Exception:
                pass  # The service may already have been closed by shutdown.
            self._close_mount(mount)
        if self._thread is not None:
            self._thread.join(timeout=1)
