//! Recorded video admission and protocol projection. No native Responses video part
//! exists: Pi's image mapping is `packages/ai/src/api/openai-responses-shared.ts:93-109`, with
//! ImageContent defined in `packages/ai/src/types.ts:438-442` in the Pi reference.
//! Decoding belongs here rather than in the Pi adapter: native providers and
//! durable sessions must also work without an extension process.
use crate::{AiError, Media, Message, Modality, Request, ToolResultPart, UserPart, VideoMedia};
use bytes::Bytes;
use std::{
    borrow::Cow,
    fs,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

/// Maximum original video size admitted to a session (10 MiB).
pub const MAX_VIDEO_BYTES: usize = 10 * 1024 * 1024;
/// Number of midpoint samples, at 1/8, 3/8, 5/8 and 7/8 of the duration.
pub const VIDEO_FRAMES: usize = 4;
/// Maximum width and height of each sampled frame.
pub const MAX_FRAME_EDGE: u32 = 768;
/// Total encoded JPEG frame budget per video.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(30);
const FORMATS: &str = "mov,matroska,webm,avi,mpeg,mpegts,ogg,asf,flv";

/// A timestamped image derived from the original video.
#[derive(Clone, Debug)]
pub struct VideoFrame {
    /// Requested presentation timestamp in seconds from the start.
    pub seconds: f64,
    /// JPEG image bytes.
    pub image: crate::ImageMedia,
}

type PreparedFrames = Result<Arc<Vec<VideoFrame>>, VideoPreparationError>;

#[derive(Debug, Default)]
pub(crate) struct FramePreparation {
    // Never held while decoding or waiting for a worker.
    outcome: Mutex<Option<(Bytes, PreparedFrames)>>,
    worker: Arc<tokio::sync::Mutex<()>>,
}

pub(crate) type FrameCache = Arc<FramePreparation>;

/// A local, payload-free video preparation failure.
#[derive(Clone, Debug, thiserror::Error)]
#[error("video preparation: {0}")]
pub struct VideoPreparationError(&'static str);

impl From<VideoPreparationError> for AiError {
    fn from(error: VideoPreparationError) -> Self {
        crate::ConfigError::VideoPreparation(error).into()
    }
}

fn error(message: &'static str) -> VideoPreparationError {
    VideoPreparationError(message)
}

const TIMED_OUT: &str = "decoder exceeded the 30-second preparation deadline; use a shorter clip";
const CANCELLED: &str = "cancelled; read or attach the clip again to retry frame preparation";

struct CancelPreparation {
    video: VideoMedia,
    cancelled: Arc<AtomicBool>,
    completed: bool,
    deadline: Instant,
}

impl Drop for CancelPreparation {
    fn drop(&mut self) {
        if !self.completed {
            self.cancelled.store(true, Ordering::Release);
            self.video
                .store_frames(Err(error(if Instant::now() >= self.deadline {
                    TIMED_OUT
                } else {
                    CANCELLED
                })));
        }
    }
}

/// Recognized local video containers. Content is validated by the decoder when
/// frames are needed; recognizing a suffix does not establish codec support.
pub fn video_mime(extension: &str) -> Option<mime::Mime> {
    Some(
        match extension.to_ascii_lowercase().as_str() {
            "mp4" | "m4v" => "video/mp4",
            "mov" => "video/quicktime",
            "webm" => "video/webm",
            "mkv" => "video/x-matroska",
            "avi" => "video/x-msvideo",
            "mpeg" | "mpg" => "video/mpeg",
            "wmv" => "video/x-ms-wmv",
            "flv" => "video/x-flv",
            "3gp" => "video/3gpp",
            "ogv" => "video/ogg",
            _ => return None,
        }
        .parse()
        .expect("static video MIME"),
    )
}

/// Validate raw-byte admission independently of the selected wire route.
pub fn validate_video(video: &VideoMedia) -> Result<(), AiError> {
    if video.data.is_empty() || video.data.len() > MAX_VIDEO_BYTES {
        return Err(error("video must contain 1 byte to 10 MiB; use a smaller clip").into());
    }
    if !matches!(video.media_type.type_().as_str(), "video") {
        return Err(error("expected a video MIME type").into());
    }
    Ok(())
}

impl VideoMedia {
    fn prepared_frames(&self) -> Option<PreparedFrames> {
        self.frames
            .outcome
            .lock()
            .expect("video frame cache")
            .as_ref()
            .filter(|(source, _)| source == &self.data)
            .map(|(_, result)| result.clone())
    }

    fn store_frames(&self, result: PreparedFrames) {
        *self.frames.outcome.lock().expect("video frame cache") = Some((self.data.clone(), result));
    }

    /// Already prepared frames only; presentation and context accounting never
    /// wait for decoders. Bytes changed by an embedder invalidate the cache.
    pub fn cached_frames(&self) -> Option<Arc<Vec<VideoFrame>>> {
        self.prepared_frames().and_then(Result::ok)
    }

    /// Consume prepared state only. Synchronous lowering/encoding never starts
    /// subprocesses, including when replaying a cold or failed video.
    pub fn sampled_frames(&self) -> Result<Arc<Vec<VideoFrame>>, AiError> {
        self.prepared_frames()
            .unwrap_or_else(|| {
                Err(error(
                    "frames not prepared; prepare the clip before encoding",
                ))
            })
            .map_err(Into::into)
    }

    /// Prepare once per immutable source off the driver, caching failures too.
    /// At most two decoders run per process; queueing and decoding share one
    /// 30-second deadline. Dropping this future stops and reaps its subprocess.
    /// Resume uses durable bytes, never the original path.
    pub async fn prepare_frames(&self) -> Result<Arc<Vec<VideoFrame>>, AiError> {
        validate_video(self)?;
        if let Some(result) = self.prepared_frames() {
            return result.map_err(Into::into);
        }
        static WORKERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
        let deadline = Instant::now() + DEADLINE;
        let result = tokio::time::timeout(DEADLINE, async {
            let source = Arc::clone(&self.frames.worker).lock_owned().await;
            if let Some(result) = self.prepared_frames() {
                return result;
            }
            let cancelled = Arc::new(AtomicBool::new(false));
            let mut guard = CancelPreparation {
                video: self.clone(),
                cancelled: Arc::clone(&cancelled),
                completed: false,
                deadline,
            };
            let permit = WORKERS.acquire().await.expect("video workers stay open");
            let video = self.clone();
            let worker = tokio::task::spawn_blocking(move || {
                let (_source, _permit) = (source, permit);
                let result = sample(&video, deadline, &cancelled).map(Arc::new);
                // Publication and cancellation use the same short cache lock;
                // a late worker cannot overwrite its cancelled owner's outcome.
                let mut cache = video.frames.outcome.lock().expect("video frame cache");
                if !cancelled.load(Ordering::Acquire) {
                    *cache = Some((video.data.clone(), result.clone()));
                }
                result
            })
            .await
            .unwrap_or_else(|_| {
                Err(error(
                    "decoder worker failed; read or attach the clip again",
                ))
            });
            self.store_frames(worker.clone());
            guard.completed = true;
            worker
        })
        .await;
        match result {
            Ok(result) => result.map_err(Into::into),
            Err(_) => Err(error(TIMED_OUT).into()),
        }
    }
}

// Spawn directly, never through a shell. Only local containers are demuxed;
// playlists/network protocols cannot turn an admitted clip into a remote read.
fn run(
    command: &mut Command,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<(), VideoPreparationError> {
    check_preparation(deadline, cancelled)?;
    let mut child = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| error("install ffmpeg and ffprobe on PATH to send video as frames"))?;
    loop {
        if let Err(error) = check_preparation(deadline, cancelled) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(error(
                        "decoder rejected the clip; use a valid, smaller local video",
                    ))
                }
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error(TIMED_OUT));
            }
        }
    }
}

fn check_preparation(
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<(), VideoPreparationError> {
    if cancelled.load(Ordering::Acquire) {
        Err(error(CANCELLED))
    } else if Instant::now() >= deadline {
        Err(error(TIMED_OUT))
    } else {
        Ok(())
    }
}

fn sample(
    video: &VideoMedia,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<Vec<VideoFrame>, VideoPreparationError> {
    check_preparation(deadline, cancelled)?;
    let dir = tempfile::tempdir()
        .map_err(|_| error("cannot create private decoder scratch directory"))?;
    let input = dir.path().join("input");
    fs::write(&input, &video.data).map_err(|_| error("cannot write decoder input"))?;
    let metadata = dir.path().join("metadata");
    let output =
        fs::File::create(&metadata).map_err(|_| error("cannot create decoder metadata"))?;
    run(
        Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-max_alloc",
                "67108864",
                "-max_pixels",
                "16000000",
                "-protocol_whitelist",
                "file,pipe",
                "-format_whitelist",
                FORMATS,
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height:format=duration",
                "-of",
                "json",
            ])
            .arg(&input)
            .stdout(output),
        deadline,
        cancelled,
    )?;
    if fs::metadata(&metadata)
        .map_err(|_| error("missing video metadata"))?
        .len()
        > 4096
    {
        return Err(error("video metadata exceeds safety limit"));
    }
    let info: serde_json::Value = serde_json::from_slice(
        &fs::read(metadata).map_err(|_| error("cannot read video metadata"))?,
    )
    .map_err(|_| error("invalid video metadata"))?;
    let duration = info["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n > 0.0 && *n <= 86400.0)
        .ok_or_else(|| error("video needs a finite duration of at most 24 hours"))?;
    let width = info["streams"][0]["width"].as_u64().unwrap_or(0);
    let height = info["streams"][0]["height"].as_u64().unwrap_or(0);
    if width == 0 || height == 0 || width.saturating_mul(height) > crate::MAX_USER_IMAGE_PIXELS {
        return Err(error(
            "video exceeds the 16-million-pixel source frame limit",
        ));
    }
    let mut frames = Vec::with_capacity(VIDEO_FRAMES);
    let mut total = 0usize;
    for index in 0..VIDEO_FRAMES {
        let seconds = duration * (2 * index + 1) as f64 / (2 * VIDEO_FRAMES) as f64;
        let output = dir.path().join("frame.jpg");
        run(
            Command::new("ffmpeg")
                .args([
                    "-nostdin",
                    "-v",
                    "error",
                    "-y",
                    "-max_alloc",
                    "67108864",
                    "-max_pixels",
                    "16000000",
                    "-threads",
                    "1",
                    "-protocol_whitelist",
                    "file,pipe",
                    "-format_whitelist",
                    FORMATS,
                    "-ss",
                ])
                .arg(format!("{seconds:.6}"))
                .arg("-i")
                .arg(&input)
                .args([
                    "-map",
                    "0:v:0",
                    "-an",
                    "-sn",
                    "-dn",
                    "-frames:v",
                    "1",
                    "-vf",
                    "scale=768:768:force_original_aspect_ratio=decrease",
                    "-threads",
                    "1",
                    "-q:v",
                    "3",
                    "-fs",
                    "1048576",
                ])
                .arg(&output)
                .stdout(Stdio::null()),
            deadline,
            cancelled,
        )?;
        let len = fs::metadata(&output)
            .map_err(|_| error("video has no frame at a sampled timestamp"))?
            .len() as usize;
        total = total.saturating_add(len);
        if len > MAX_FRAME_BYTES / VIDEO_FRAMES || total > MAX_FRAME_BYTES {
            return Err(error("sampled frames exceed the 4 MiB output limit"));
        }
        let data = fs::read(&output).map_err(|_| error("cannot read sampled frame"))?;
        let Media::Image(image) = Media::image_bytes(Bytes::from(data), mime::IMAGE_JPEG) else {
            unreachable!()
        };
        let image = crate::prepare_user_image(
            &image,
            crate::ImageInputLimits {
                max_width: MAX_FRAME_EDGE,
                max_height: MAX_FRAME_EDGE,
                max_bytes: MAX_FRAME_BYTES / VIDEO_FRAMES,
            },
        )
        .map_err(|_| error("decoder produced an invalid or oversized frame"))?;
        frames.push(VideoFrame { seconds, image });
        fs::remove_file(output).map_err(|_| error("cannot remove sampled frame scratch"))?;
    }
    Ok(frames)
}

/// Prepare video sources before synchronous request/replay projection. Failed
/// sources remain in canonical history and project an actionable placeholder.
/// Native video and text-only routes never start a decoder. Returns whether
/// cold state was encountered, so owners can invalidate derived context sizes.
pub async fn prepare_message_frames(model: &crate::Model, messages: &[Message]) -> bool {
    if model.spec.supports_native_video() || !model.spec.supports_video_input() {
        return false;
    }
    let mut changed = false;
    for message in messages {
        if let Message::User(user) = message {
            for part in &user.content {
                match part {
                    UserPart::Media(Media::Video(video)) => {
                        changed |= video.prepared_frames().is_none();
                        let _ = video.prepare_frames().await;
                    }
                    UserPart::ToolResult(result) => {
                        for part in &result.content {
                            if let ToolResultPart::Media(Media::Video(video)) = part {
                                changed |= video.prepared_frames().is_none();
                                let _ = video.prepare_frames().await;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    changed
}

/// An explicit Responses window already owns the wire input. Canonical media
/// excluded by that window is retained for history, not prepared for this call.
pub(crate) async fn prepare_request_frames(model: &crate::Model, request: &Request) {
    if model.spec.protocol == crate::Protocol::OpenAiResponses
        && request
            .responses
            .as_ref()
            .is_some_and(|options| options.input.is_some())
    {
        return;
    }
    prepare_message_frames(model, &request.messages).await;
}

/// Project owned host requests without cloning conversation history. Native
/// codecs project borrowed user messages during their existing encoding walk.
pub(crate) fn prepare_request_owned(
    model: &crate::Model,
    mut request: Request,
) -> Result<Request, AiError> {
    for message in &mut request.messages {
        if let Message::User(user) = message {
            if let Cow::Owned(projected) = prepare_user_message(model, user)? {
                *user = projected;
            }
        }
    }
    Ok(request)
}

/// Pi's image contract is `packages/coding-agent/src/core/tools/read.ts:128-149`;
/// video projection extends native media without inventing a Pi video block.
pub(crate) fn prepare_user_message<'a>(
    model: &crate::Model,
    user: &'a crate::UserMessage,
) -> Result<Cow<'a, crate::UserMessage>, AiError> {
    let has_video = user.content.iter().any(|part| match part {
        UserPart::Media(Media::Video(_)) => true,
        UserPart::ToolResult(result) => result
            .content
            .iter()
            .any(|part| matches!(part, ToolResultPart::Media(Media::Video(_)))),
        _ => false,
    });
    if !has_video {
        return Ok(Cow::Borrowed(user));
    }
    let mut content = Vec::new();
    for part in &user.content {
        match part {
            UserPart::Media(Media::Video(video)) => content.extend(project(model, video)?),
            UserPart::ToolResult(result) => {
                let mut result = result.clone();
                let mut adjacent = Vec::new();
                result.content.retain(|part| {
                    if let ToolResultPart::Media(Media::Video(video)) = part {
                        adjacent.push(video.clone());
                        false
                    } else {
                        true
                    }
                });
                content.push(UserPart::ToolResult(result));
                for video in adjacent {
                    content.extend(project(model, &video)?);
                }
            }
            _ => content.push(part.clone()),
        }
    }
    Ok(Cow::Owned(crate::UserMessage { content }))
}

fn project(model: &crate::Model, video: &VideoMedia) -> Result<Vec<UserPart>, AiError> {
    validate_video(video)?;
    if model.spec.supports_native_video() {
        return Ok(vec![UserPart::Media(Media::Video(video.clone()))]);
    }
    if !model
        .spec
        .capabilities
        .input_modalities
        .contains(Modality::Image)
    {
        return Err(crate::UnsupportedError::Video.into());
    }
    let frames = match video.sampled_frames() {
        Ok(frames) => frames,
        Err(error) => {
            return Ok(vec![UserPart::Text(format!(
                "{error}; provider {} model {} continues without video frames; original retained",
                model.endpoint.id.0, model.spec.api_name
            ))])
        }
    };
    let mut parts = vec![UserPart::Text(format!(
        "video sent as {} frames (sampled visual content only; no audio)",
        frames.len()
    ))];
    for frame in frames.iter() {
        parts.push(UserPart::Text(format!(
            "[video frame at {:.3}s]",
            frame.seconds
        )));
        let image = if let Some(limits) = model.spec.preset.image_input_limits {
            crate::prepare_user_image(&frame.image, limits)
                .map_err(|_| error("frame cannot fit this model's image limits"))?
        } else {
            frame.image.clone()
        };
        parts.push(UserPart::Media(Media::Image(image)));
    }
    Ok(parts)
}

#[cfg(test)]
mod tests;
