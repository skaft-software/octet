//! Payload-free native image nodes; disclosed validated blobs are sent only at the sink.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD;
use octet_tern::{
    client::TernClient,
    wire::{Kind, Node, Props},
};
use sexy_tui_rs::images::{ImageFormat, TerminalImage};
use sha2::{Digest, Sha256};

use super::{ShellState, TranscriptBlock};

struct Image {
    payload: Arc<TerminalImage>,
    address: String,
    disclosed: bool,
}

#[derive(Default)]
pub(super) struct NativeImages {
    images: HashMap<String, Image>,
    uploaded: HashSet<String>,
}

struct HashWriter(Sha256);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl NativeImages {
    pub(super) fn prepare(
        &mut self,
        shell: &ShellState,
        supported: bool,
        verbose: bool,
    ) -> io::Result<()> {
        let mut retained = HashSet::new();
        for image in self.images.values_mut() {
            image.disclosed = false;
        }
        if supported {
            for (index, block) in shell.transcript.iter().enumerate() {
                let TranscriptBlock::Tool(panel) = block else {
                    continue;
                };
                // Command output is private until Ctrl+O disclosure. Gate
                // payload preparation too, not just the native image node:
                // upload runs before the projected tree is sent to Tern.
                if !panel.image_rendering.enabled
                    || (matches!(panel.name.as_str(), "bash" | "exec") && !shell.verbose_tools)
                {
                    continue;
                }
                for (image_index, image) in panel.images.iter().enumerate() {
                    let Some(payload) = image.terminal_image() else {
                        continue;
                    };
                    let id = format!("t{}.image{image_index}", shell.transcript_commit_ids[index]);
                    retained.insert(id.clone());
                    // Command images are captured output, not a native preview.
                    // Do not hash, mount or transport them before global Ctrl+O.
                    // Keep previously disclosed addresses cached across hiding so
                    // revealing the same content does not upload it again.
                    if matches!(panel.name.as_str(), "bash" | "exec") && !verbose {
                        continue;
                    }
                    if let Some(image) = self.images.get_mut(&id) {
                        if Arc::ptr_eq(&image.payload, &payload) {
                            image.disclosed = true;
                            continue;
                        }
                    }
                    let mut hash = HashWriter(Sha256::new());
                    payload.write_payload_to(&mut hash)?;
                    self.images.insert(
                        id,
                        Image {
                            payload,
                            address: format!("{:x}", hash.0.finalize()),
                            disclosed: true,
                        },
                    );
                }
            }
        }
        self.images.retain(|id, _| retained.contains(id));
        self.uploaded
            .retain(|address| self.images.values().any(|image| &image.address == address));
        Ok(())
    }

    pub(super) fn upload(&mut self, client: &mut TernClient) -> io::Result<()> {
        for image in self.images.values().filter(|image| image.disclosed) {
            if self.uploaded.contains(&image.address) {
                continue;
            }
            let mime = match image.payload.format() {
                ImageFormat::Png => "image/png",
                ImageFormat::Jpeg => "image/jpeg",
                ImageFormat::Gif => "image/gif",
                ImageFormat::Webp => "image/webp",
            };
            let mut encoded = base64::write::EncoderWriter::new(Vec::new(), &STANDARD);
            image.payload.write_payload_to(&mut encoded)?;
            let encoded = String::from_utf8(encoded.finish()?).expect("base64 is ASCII");
            client.blob(&image.address, mime, &encoded)?;
            self.uploaded.insert(image.address.clone());
        }
        Ok(())
    }

    pub(super) fn node(&self, id: &str) -> Option<Node> {
        let image = self.images.get(id).filter(|image| image.disclosed)?;
        let dimensions = image.payload.dimensions();
        Some(Node::new(
            id,
            Kind::Image,
            Props::new()
                .role("octet.tool.image")
                .set("blob", &image.address)
                .set(
                    "alt",
                    format!(
                        "{} image, {} × {}",
                        image.payload.format().name(),
                        dimensions.width(),
                        dimensions.height()
                    ),
                )
                .set("w", dimensions.width())
                .set("h", dimensions.height())
                .set("max", serde_json::json!({"w":1.0,"h":"24lines"})),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{summarize_tool, ShellOutput, ToolPanel};
    use super::*;
    use crate::hydrate::{ToolImagePlaceholder, ToolResultImage};
    use base64::Engine;
    use octet_ai::ToolCallId;
    use sexy_tui_rs::images::ImageCapabilities;
    use std::sync::Mutex;

    const PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4,
        0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 248, 15, 0, 1, 5,
        1, 1, 39, 24, 227, 102, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    #[derive(Clone, Default)]
    struct Output(Arc<Mutex<Vec<String>>>);
    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap()
                .push(String::from_utf8(bytes.to_vec()).unwrap());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Output {
        fn blobs(&self) -> Vec<octet_tern::frame::Raw> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter_map(|wire| octet_tern::frame::split(wire).filter(|raw| raw.verb == "b"))
                .collect()
        }
    }

    fn sink() -> (TernClient, Output) {
        let output = Output::default();
        let client = TernClient::with_writer("test", None, output.clone()).unwrap();
        (client, output)
    }

    fn fixture(name: &str, enabled: bool) -> (ShellState, usize, [String; 2]) {
        let mut shell = ShellState::default();
        shell.update_image_rendering(enabled, ImageCapabilities::forced(None, None));
        let args = serde_json::json!({"command":"capture", "path":"pixel.png"});
        let mut panel = ToolPanel::new(
            ToolCallId("image-output".into()),
            name.into(),
            args.to_string(),
            summarize_tool(name, &args),
            "captured output".into(),
            false,
            false,
            None,
            None,
        );
        // Independently validated payloads with identical content must deduplicate.
        panel.images = vec![
            ToolResultImage::Ready {
                image: Arc::new(TerminalImage::from_slice(PNG).unwrap()),
                id: None,
            },
            ToolResultImage::Ready {
                image: Arc::new(TerminalImage::from_slice(PNG).unwrap()),
                id: None,
            },
            ToolResultImage::Placeholder(ToolImagePlaceholder::Invalid),
        ];
        let index = shell.push_block(TranscriptBlock::Tool(Box::new(panel)));
        let identity = shell.transcript_commit_ids[index];
        (
            shell,
            index,
            [format!("t{identity}.image0"), format!("t{identity}.image1")],
        )
    }

    #[test]
    fn hidden_command_images_never_prepare_or_upload_in_any_lifecycle_state() {
        for name in ["bash", "exec"] {
            let (mut shell, index, ids) = fixture(name, true);
            let budget = shell.tool_image_budget.clone();
            let mut images = NativeImages::default();
            let (mut client, output) = sink();
            for (finished, is_error, text) in [
                (false, false, ""),
                (false, false, "partial captured output"),
                (true, false, "complete captured output"),
                (true, true, "failed captured output"),
            ] {
                let TranscriptBlock::Tool(panel) = &mut shell.transcript[index] else {
                    panic!("tool fixture")
                };
                panel.finished = finished;
                panel.is_error = is_error;
                panel.output = text.into();
                images.prepare(&shell, true, shell.verbose_tools).unwrap();
                images.upload(&mut client).unwrap();
                assert!(images.images.is_empty(), "hidden images must not be hashed");
                assert!(images.uploaded.is_empty());
                assert!(ids.iter().all(|id| images.node(id).is_none()));
                assert!(output.blobs().is_empty(), "{name}, {finished}, {is_error}");
                assert_eq!(shell.tool_image_budget, budget);
                let TranscriptBlock::Tool(panel) = &shell.transcript[index] else {
                    panic!("tool fixture")
                };
                assert_eq!(panel.output, text);
                assert_eq!(panel.images.len(), 3);
                assert_eq!(panel.images[0].byte_len(), Some(PNG.len()));
            }
        }
    }

    #[test]
    fn command_images_reveal_hide_and_reveal_without_reuploading_content() {
        for name in ["bash", "exec"] {
            let (mut shell, _, ids) = fixture(name, true);
            let mut images = NativeImages::default();
            let (mut client, output) = sink();
            for (step, verbose) in [false, true, false, true].into_iter().enumerate() {
                shell.verbose_tools = verbose;
                images.prepare(&shell, true, shell.verbose_tools).unwrap();
                images.upload(&mut client).unwrap();
                for id in &ids {
                    assert_eq!(images.node(id).is_some(), verbose);
                }
                assert_eq!(output.blobs().len(), usize::from(step > 0));
                if verbose {
                    assert_eq!(images.images.len(), 2, "rejected payload is not admitted");
                    let expected = format!("{:x}", Sha256::digest(PNG));
                    for id in &ids {
                        let node = images.node(id).unwrap();
                        assert_eq!(node.k, Kind::Image);
                        let props = node.p.as_ref().unwrap().as_map();
                        assert_eq!(props["blob"], expected);
                        assert_eq!(props["w"], 1);
                        assert_eq!(props["h"], 1);
                        assert!(!serde_json::to_string(&node)
                            .unwrap()
                            .contains(&STANDARD.encode(PNG)));
                    }
                    let blobs = output.blobs();
                    assert_eq!(blobs[0].params["mime"], "image/png");
                    assert_eq!(STANDARD.decode(&blobs[0].body).unwrap(), PNG);
                }
            }
            // Transcript retirement still releases cache and upload bookkeeping.
            images.prepare(&ShellState::default(), true, false).unwrap();
            assert!(images.images.is_empty());
            assert!(images.uploaded.is_empty());
        }
    }

    #[test]
    fn hiding_prepared_but_unsent_command_images_prevents_transport() {
        let (shell, _, ids) = fixture("bash", true);
        let mut images = NativeImages::default();
        let (mut client, output) = sink();
        images.prepare(&shell, true, true).unwrap();
        assert!(ids.iter().all(|id| images.node(id).is_some()));
        // Preparation is not permission to upload after a later hide.
        images.prepare(&shell, true, false).unwrap();
        images.upload(&mut client).unwrap();
        assert!(ids.iter().all(|id| images.node(id).is_none()));
        assert!(output.blobs().is_empty());
        images.prepare(&shell, true, true).unwrap();
        images.upload(&mut client).unwrap();
        assert_eq!(output.blobs().len(), 1);
    }

    #[test]
    fn noncommand_images_keep_existing_opt_in_admission_and_deduplication() {
        for name in ["read", "other"] {
            let (mut shell, index, ids) = fixture(name, true);
            let mut images = NativeImages::default();
            let (mut client, output) = sink();
            for (verbose, finished, is_error) in [
                (false, false, false),
                (false, true, false),
                (false, true, true),
                (true, true, true),
                (false, true, true),
            ] {
                shell.verbose_tools = verbose;
                let TranscriptBlock::Tool(panel) = &mut shell.transcript[index] else {
                    panic!("tool fixture")
                };
                panel.finished = finished;
                panel.is_error = is_error;
                images.prepare(&shell, true, shell.verbose_tools).unwrap();
                images.upload(&mut client).unwrap();
                assert!(ids.iter().all(|id| images.node(id).is_some()));
                assert_eq!(images.images.len(), 2, "rejected payload is not admitted");
                assert_eq!(output.blobs().len(), 1);
            }
        }
    }

    #[test]
    fn unsupported_disabled_and_rejected_images_remain_payload_free() {
        for name in ["bash", "exec", "read"] {
            for (enabled, supported) in [(false, false), (false, true), (true, false)] {
                let (shell, index, ids) = fixture(name, enabled);
                let mut images = NativeImages::default();
                let (mut client, output) = sink();
                for verbose in [false, true, false] {
                    images.prepare(&shell, supported, verbose).unwrap();
                    images.upload(&mut client).unwrap();
                    assert!(images.images.is_empty());
                    assert!(ids.iter().all(|id| images.node(id).is_none()));
                    assert!(output.blobs().is_empty());
                    let TranscriptBlock::Tool(panel) = &shell.transcript[index] else {
                        panic!("tool fixture")
                    };
                    assert_eq!(panel.images[0].byte_len(), Some(PNG.len()));
                    assert_eq!(
                        panel.images[0].fallback_text(!enabled),
                        if enabled {
                            "[image: PNG 1x1 (unavailable)]"
                        } else {
                            "[image: PNG 1x1 (hidden; enable terminal images)]"
                        }
                    );
                    assert_eq!(
                        panel.images[2].fallback_text(!enabled),
                        "[image unavailable: invalid inline payload]"
                    );
                    let identity = shell.transcript_commit_ids[index];
                    assert!(images.node(&format!("t{identity}.image2")).is_none());
                }
            }
        }
    }

    #[test]
    fn previously_prepared_images_are_retired_when_admission_is_removed() {
        for (enabled, supported, rejected) in [
            (false, true, false),
            (true, false, false),
            (true, true, true),
        ] {
            let (mut shell, index, ids) = fixture("bash", true);
            let mut images = NativeImages::default();
            let (mut client, output) = sink();
            images.prepare(&shell, true, true).unwrap();
            assert!(ids.iter().all(|id| images.node(id).is_some()));
            shell.update_image_rendering(enabled, ImageCapabilities::forced(None, None));
            if rejected {
                let TranscriptBlock::Tool(panel) = &mut shell.transcript[index] else {
                    panic!("tool fixture")
                };
                for image in &mut panel.images {
                    *image = ToolResultImage::Placeholder(ToolImagePlaceholder::Invalid);
                }
            }
            images.prepare(&shell, supported, true).unwrap();
            images.upload(&mut client).unwrap();
            assert!(images.images.is_empty());
            assert!(images.uploaded.is_empty());
            assert!(ids.iter().all(|id| images.node(id).is_none()));
            assert!(output.blobs().is_empty());
        }
    }

    #[test]
    fn local_shell_blocks_cannot_admit_image_payloads() {
        let mut shell = ShellState::default();
        let index = shell.push_block(TranscriptBlock::Shell(Box::new(ShellOutput {
            id: "local".into(),
            command: "capture".into(),
            output: "captured output".into(),
            exit_code: 0,
            running: true,
        })));
        let mut images = NativeImages::default();
        let (mut client, output) = sink();
        for (running, exit_code) in [(true, 0), (false, 0), (false, 1)] {
            let TranscriptBlock::Shell(result) = &mut shell.transcript[index] else {
                panic!("shell fixture")
            };
            result.running = running;
            result.exit_code = exit_code;
            for verbose in [false, true, false] {
                images.prepare(&shell, true, verbose).unwrap();
                images.upload(&mut client).unwrap();
                assert!(images.images.is_empty());
                assert!(output.blobs().is_empty());
            }
            let TranscriptBlock::Shell(result) = &shell.transcript[index] else {
                panic!("shell fixture")
            };
            assert_eq!(result.output, "captured output");
        }
    }
}
