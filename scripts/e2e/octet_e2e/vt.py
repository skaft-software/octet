"""A bounded VT observer for the octet interactive TUI.

The suite drives the real binary through a PTY and asserts on what a terminal
would show. This is deliberately not a general terminal emulator: it implements
the control surface octet's renderer actually emits (primary/alternate screen,
cursor addressing, erase/insert/delete, SGR including 24-bit colour, scroll
regions, OSC clipboard/title/hyperlink, bracketed paste and mouse-mode flags)
plus bounded scrollback for diagnostics. Wide characters occupy two cells;
combining characters are folded into the previous cell.
"""

from __future__ import annotations

import codecs
import unicodedata
from dataclasses import dataclass, replace


@dataclass(frozen=True)
class Style:
    fg: tuple[int, int, int] | None = None
    bg: tuple[int, int, int] | None = None
    bold: bool = False
    italic: bool = False
    underline: bool = False
    reverse: bool = False
    dim: bool = False
    strike: bool = False


class Screen:
    """Terminal state after feeding a byte stream."""

    def __init__(self, columns: int = 120, rows: int = 40, scrollback_limit: int = 4000):
        self.columns = columns
        self.rows = rows
        self.scrollback_limit = scrollback_limit
        self.scrollback: list[str] = []
        self.osc52: list[str] = []
        self.title: str | None = None
        self.responses: list[bytes] = []
        self.frames = 0
        self._visible = True
        self._reset_buffers()
        self._decoder = codecs.getincrementaldecoder("utf-8")("replace")

    # ----------------------------------------------------------------- state
    def _reset_buffers(self) -> None:
        self.grid = [[" "] * self.columns for _ in range(self.rows)]
        self.styles = [[Style()] * self.columns for _ in range(self.rows)]
        self.alt_grid = None
        self.alt_styles = None
        self.x = 0
        self.y = 0
        self.saved = (0, 0)
        self.scroll_top = 0
        self.scroll_bottom = self.rows - 1
        self.autowrap = True
        self.origin = False
        self.insert = False
        self.pending_wrap = False
        self.style = Style()
        self.mouse_modes: set[int] = set()
        self.bracketed_paste = False
        self.state = "text"
        self.sequence = ""
        self.osc_buffer = ""

    def resize(self, columns: int, rows: int) -> None:
        def reshape(grid, fill):
            grid = [row[:columns] + [fill] * max(0, columns - len(row)) for row in grid[:rows]]
            grid += [[fill] * columns for _ in range(rows - len(grid))]
            return grid

        self.grid = reshape(self.grid, " ")
        self.styles = reshape(self.styles, Style())
        if self.alt_grid is not None:
            self.alt_grid = reshape(self.alt_grid, " ")
            self.alt_styles = reshape(self.alt_styles, Style())
        self.columns = columns
        self.rows = rows
        self.scroll_top = 0
        self.scroll_bottom = rows - 1
        self.x = min(self.x, columns - 1)
        self.y = min(self.y, rows - 1)

    # -------------------------------------------------------------- queries
    def text(self) -> str:
        return "\n".join("".join(row).rstrip() for row in self.grid)

    def row(self, y: int) -> str:
        if not 0 <= y < self.rows:
            return ""
        return "".join(self.grid[y]).rstrip()

    def cell(self, x: int, y: int) -> tuple[str, Style]:
        if not (0 <= x < self.columns and 0 <= y < self.rows):
            return " ", Style()
        return (self.grid[y][x] or " ", self.styles[y][x])

    def find(self, needle: str) -> bool:
        return needle in self.text()

    def rows_containing(self, needle: str) -> list[int]:
        return [y for y in range(self.rows) if needle in self.row(y)]

    def scrollback_text(self) -> str:
        return "\n".join(self.scrollback)

    # ---------------------------------------------------------------- input
    def feed(self, data: bytes) -> None:
        for char in self._decoder.decode(data):
            self._feed_char(char)

    def _feed_char(self, char: str) -> None:
        state = self.state
        if state == "osc":
            if char == "\a":
                self._finish_osc()
            elif char == "\x1b":
                self.state = "osc_escape"
            else:
                self.osc_buffer += char
            return
        if state == "osc_escape":
            if char == "\\":
                self._finish_osc()
            else:
                self.osc_buffer += char
                self.state = "osc"
            return
        if state == "escape":
            if char == "[":
                self.state, self.sequence = "csi", ""
            elif char in "]P_^":
                self.state, self.osc_buffer = "osc", ""
            elif char == "7":
                self.saved, self.state = (self.x, self.y), "text"
            elif char == "8":
                self.x, self.y = self.saved
                self.state = "text"
            elif char == "(":
                self.state = "charset"
            elif char == ")" or char == "*" or char == "+":
                self.state = "charset"
            elif char == "=" or char == ">":
                self.state = "text"
            elif char == "D":
                self._line_feed()
                self.state = "text"
            elif char == "M":
                self._reverse_index()
                self.state = "text"
            elif char == "E":
                self.x = 0
                self._line_feed()
                self.state = "text"
            elif char == "c":
                self.hard_reset()
                self.state = "text"
            else:
                self.state = "text"
            return
        if state == "charset":
            self.state = "text"
            return
        if state == "csi":
            if char == "\x1b":
                self.state = "escape"
            elif "@" <= char <= "~":
                self._csi(self.sequence, char)
                self.state = "text"
            else:
                self.sequence += char
            return
        if char == "\x1b":
            self.state = "escape"
        elif char == "\r":
            self.x = 0
            self.pending_wrap = False
        elif char in ("\n", "\v", "\f"):
            self._line_feed()
        elif char == "\b":
            self.pending_wrap = False
            self.x = max(0, self.x - 1)
        elif char == "\t":
            self.pending_wrap = False
            self.x = min(self.columns - 1, (self.x // 8 + 1) * 8)
        elif char == "\x07" or char == "\x00":
            pass
        elif ord(char) >= 32 and char != "\x7f":
            self._print(char)
        # Other C0 controls are ignored, matching a quiet terminal.

    def _print(self, char: str) -> None:
        if unicodedata.combining(char):
            x = self.x - 1 if self.pending_wrap else self.x
            x = max(0, x - 1)
            if self.grid[self.y][x] not in ("", " "):
                self.grid[self.y][x] += char
            return
        width = 2 if unicodedata.east_asian_width(char) in ("W", "F") else 1
        if self.pending_wrap:
            self.x = 0
            self._line_feed()
            self.pending_wrap = False
        if self.x + width > self.columns:
            if self.autowrap:
                self.x = 0
                self._line_feed()
            else:
                self.x = self.columns - width
        self.grid[self.y][self.x] = char
        self.styles[self.y][self.x] = self.style
        if width == 2 and self.x + 1 < self.columns:
            self.grid[self.y][self.x + 1] = ""
            self.styles[self.y][self.x + 1] = self.style
        self.x += width
        if self.x >= self.columns:
            if self.autowrap:
                self.x = self.columns - 1
                self.pending_wrap = True
            else:
                self.x = self.columns - 1

    # -------------------------------------------------------------- movement
    def _line_feed(self) -> None:
        if self.y == self.scroll_bottom:
            self._scroll_up(1)
        else:
            self.y = min(self.rows - 1, self.y + 1)

    def _reverse_index(self) -> None:
        if self.y == self.scroll_top:
            self._scroll_down(1)
        else:
            self.y = max(0, self.y - 1)

    def _scroll_up(self, count: int) -> None:
        for _ in range(count):
            line = "".join(self.grid[self.scroll_top]).rstrip()
            if self.scroll_top == 0:
                self.scrollback.append(line)
                if len(self.scrollback) > self.scrollback_limit:
                    del self.scrollback[: len(self.scrollback) - self.scrollback_limit]
            del self.grid[self.scroll_top]
            del self.styles[self.scroll_top]
            self.grid.insert(self.scroll_bottom, [" "] * self.columns)
            self.styles.insert(self.scroll_bottom, [Style()] * self.columns)

    def _scroll_down(self, count: int) -> None:
        for _ in range(count):
            del self.grid[self.scroll_bottom]
            del self.styles[self.scroll_bottom]
            self.grid.insert(self.scroll_top, [" "] * self.columns)
            self.styles.insert(self.scroll_top, [Style()] * self.columns)

    def _erase_cells(self, y: int, start: int, end: int) -> None:
        for x in range(max(0, start), min(self.columns, end)):
            self.grid[y][x] = " "
            self.styles[y][x] = Style()

    # ------------------------------------------------------------------- CSI
    def _csi(self, body: str, final: str) -> None:
        private = body[:1] in ("?", ">", "<", "=")
        prefix = body[0] if private else ""
        params = body[1:] if private else body
        nums: list[int] = []
        for part in params.split(";"):
            if part == "":
                nums.append(0)
            else:
                digits = "".join(c for c in part if c.isdigit())
                nums.append(int(digits) if digits else 0)
        n = nums[0] if nums else 0

        if private:
            self._private_mode(prefix, nums, final)
            return

        if final in "Hf":
            row = (nums[0] if nums and nums[0] else 1) - 1
            col = (nums[1] if len(nums) > 1 and nums[1] else 1) - 1
            if self.origin:
                row += self.scroll_top
            self.y = min(self.rows - 1, max(0, row))
            self.x = min(self.columns - 1, max(0, col))
            self.pending_wrap = False
        elif final == "A":
            self.y = max(0, self.y - (n or 1))
            self.pending_wrap = False
        elif final == "B":
            self.y = min(self.rows - 1, self.y + (n or 1))
            self.pending_wrap = False
        elif final == "C":
            self.x = min(self.columns - 1, self.x + (n or 1))
            self.pending_wrap = False
        elif final == "D":
            self.x = max(0, self.x - (n or 1))
            self.pending_wrap = False
        elif final == "E":
            self.x = 0
            self.y = min(self.rows - 1, self.y + (n or 1))
        elif final == "F":
            self.x = 0
            self.y = max(0, self.y - (n or 1))
        elif final == "G" or final == "`":
            self.x = min(self.columns - 1, max(0, (n or 1) - 1))
            self.pending_wrap = False
        elif final == "d":
            row = (n or 1) - 1 + (self.scroll_top if self.origin else 0)
            self.y = min(self.rows - 1, max(0, row))
        elif final == "J":
            if n in (2, 3):
                for y in range(self.rows):
                    self._erase_cells(y, 0, self.columns)
            elif n == 0:
                self._erase_cells(self.y, self.x, self.columns)
                for y in range(self.y + 1, self.rows):
                    self._erase_cells(y, 0, self.columns)
            elif n == 1:
                for y in range(0, self.y):
                    self._erase_cells(y, 0, self.columns)
                self._erase_cells(self.y, 0, self.x + 1)
        elif final == "K":
            if n == 0:
                self._erase_cells(self.y, self.x, self.columns)
            elif n == 1:
                self._erase_cells(self.y, 0, self.x + 1)
            elif n == 2:
                self._erase_cells(self.y, 0, self.columns)
        elif final == "L":
            for _ in range(n or 1):
                if self.scroll_top <= self.y <= self.scroll_bottom:
                    del self.grid[self.scroll_bottom]
                    del self.styles[self.scroll_bottom]
                    self.grid.insert(self.y, [" "] * self.columns)
                    self.styles.insert(self.y, [Style()] * self.columns)
        elif final == "M":
            for _ in range(n or 1):
                if self.scroll_top <= self.y <= self.scroll_bottom:
                    del self.grid[self.y]
                    del self.styles[self.y]
                    self.grid.insert(self.scroll_bottom, [" "] * self.columns)
                    self.styles.insert(self.scroll_bottom, [Style()] * self.columns)
        elif final == "P":
            count = n or 1
            row = self.grid[self.y]
            styles = self.styles[self.y]
            del row[self.x : self.x + count]
            del styles[self.x : self.x + count]
            row.extend([" "] * count)
            styles.extend([Style()] * count)
        elif final == "@":
            count = n or 1
            row = self.grid[self.y]
            styles = self.styles[self.y]
            for _ in range(count):
                row.insert(self.x, " ")
                styles.insert(self.x, Style())
            del row[self.columns :]
            del styles[self.columns :]
        elif final == "X":
            self._erase_cells(self.y, self.x, self.x + (n or 1))
        elif final == "S":
            self._scroll_up(n or 1)
        elif final == "T":
            self._scroll_down(n or 1)
        elif final == "r":
            top = (nums[0] if nums and nums[0] else 1) - 1
            bottom = (nums[1] if len(nums) > 1 and nums[1] else self.rows) - 1
            if 0 <= top < bottom < self.rows:
                self.scroll_top, self.scroll_bottom = top, bottom
            else:
                self.scroll_top, self.scroll_bottom = 0, self.rows - 1
            self.x = self.y = 0
            self.pending_wrap = False
        elif final == "m":
            self._sgr(nums)
        elif final == "s":
            self.saved = (self.x, self.y)
        elif final == "u":
            self.x, self.y = self.saved
        elif final == "n":
            if n == 5:
                self.responses.append(b"\x1b[0n")
            elif n == 6:
                self.responses.append(
                    f"\x1b[{self.y + 1};{self.x + 1}R".encode("ascii")
                )
        # Unhandled finals are intentionally ignored; the product's renderer
        # does not depend on them, and asserting on a guess would be worse.

    def _private_mode(self, prefix: str, nums: list[int], final: str) -> None:
        for mode in nums or [0]:
            if prefix == "?":
                if final == "h":
                    self._set_mode(mode, True)
                elif final == "l":
                    self._set_mode(mode, False)

    def _set_mode(self, mode: int, enabled: bool) -> None:
        if mode == 7:
            self.autowrap = enabled
        elif mode == 6:
            self.origin = enabled
            self.x = self.y = 0
        elif mode == 4:
            self.insert = enabled
        elif mode == 25:
            self._visible = enabled
        elif mode == 2004:
            self.bracketed_paste = enabled
        elif mode in (1000, 1002, 1003, 1005, 1006, 1015):
            if enabled:
                self.mouse_modes.add(mode)
            else:
                self.mouse_modes.discard(mode)
        elif mode in (1049, 47, 1047):
            if enabled:
                if self.alt_grid is None:
                    self.alt_grid, self.alt_styles = self.grid, self.styles
                    self.grid = [[" "] * self.columns for _ in range(self.rows)]
                    self.styles = [[Style()] * self.columns for _ in range(self.rows)]
                self.x = self.y = 0
            elif self.alt_grid is not None:
                self.grid, self.styles = self.alt_grid, self.alt_styles
                self.alt_grid = self.alt_styles = None
                self.x = self.y = 0
                self.pending_wrap = False

    def _sgr(self, nums: list[int]) -> None:
        if not nums:
            nums = [0]
        index = 0
        style = self.style
        while index < len(nums):
            code = nums[index]
            if code == 0:
                style = Style()
            elif code == 1:
                style = replace(style, bold=True)
            elif code == 2:
                style = replace(style, dim=True)
            elif code == 3:
                style = replace(style, italic=True)
            elif code == 4:
                style = replace(style, underline=True)
            elif code == 7:
                style = replace(style, reverse=True)
            elif code == 9:
                style = replace(style, strike=True)
            elif code == 22:
                style = replace(style, bold=False, dim=False)
            elif code == 23:
                style = replace(style, italic=False)
            elif code == 24:
                style = replace(style, underline=False)
            elif code == 27:
                style = replace(style, reverse=False)
            elif code == 29:
                style = replace(style, strike=False)
            elif code == 39:
                style = replace(style, fg=None)
            elif 30 <= code <= 37:
                style = replace(style, fg=self._basic_color(code - 30))
            elif code == 38 and index + 1 < len(nums):
                color, index = self._extended_color(nums, index + 1)
                style = replace(style, fg=color)
            elif code == 48 and index + 1 < len(nums):
                color, index = self._extended_color(nums, index + 1)
                style = replace(style, bg=color)
            elif 90 <= code <= 97:
                style = replace(style, fg=self._basic_color(code - 90 + 8))
            elif code == 49:
                style = replace(style, bg=None)
            elif 40 <= code <= 47:
                style = replace(style, bg=self._basic_color(code - 40))
            elif 100 <= code <= 107:
                style = replace(style, bg=self._basic_color(code - 100 + 8))
            index += 1
        self.style = style

    def _extended_color(self, nums: list[int], index: int) -> tuple[tuple[int, int, int] | None, int]:
        mode = nums[index] if index < len(nums) else 0
        if mode == 5 and index + 1 < len(nums):
            return self._basic_color(nums[index + 1]), index + 1
        if mode == 2 and index + 3 < len(nums):
            rgb = (nums[index + 1] & 255, nums[index + 2] & 255, nums[index + 3] & 255)
            return rgb, index + 3
        return None, index

    @staticmethod
    def _basic_color(index: int) -> tuple[int, int, int]:
        table = (
            (0, 0, 0), (205, 0, 0), (0, 205, 0), (205, 205, 0),
            (0, 0, 238), (205, 0, 205), (0, 205, 205), (229, 229, 229),
            (127, 127, 127), (255, 0, 0), (0, 255, 0), (255, 255, 0),
            (92, 92, 255), (255, 0, 255), (0, 255, 255), (255, 255, 255),
        )
        if 0 <= index < 16:
            return table[index]
        if 16 <= index <= 231:
            index -= 16
            r, g, b = index // 36, (index // 6) % 6, index % 6
            scale = lambda v: 0 if v == 0 else 40 + v * 15  # noqa: E731
            return (scale(r), scale(g), scale(b))
        level = 8 + (index - 232) * 10
        return (level, level, level)

    # ------------------------------------------------------------------- OSC
    def _finish_osc(self) -> None:
        buffer, self.osc_buffer = self.osc_buffer, ""
        self.state = "text"
        if ";" not in buffer:
            return
        kind, _, payload = buffer.partition(";")
        if kind in ("0", "1", "2"):
            self.title = payload
        elif kind == "52":
            _, _, encoded = payload.partition(";")
            if encoded:
                import base64

                try:
                    self.osc52.append(base64.b64decode(encoded).decode("utf-8", "replace"))
                except (ValueError, UnicodeError):
                    self.osc52.append("")

    def hard_reset(self) -> None:
        scrollback = self.scrollback
        self._reset_buffers()
        self.scrollback = scrollback
