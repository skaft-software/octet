//! Payload-free native image nodes; validated blobs are sent only at the sink.

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
    pub(super) fn prepare(&mut self, shell: &ShellState, supported: bool) -> io::Result<()> {
        let mut retained = HashSet::new();
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
                    if self
                        .images
                        .get(&id)
                        .is_some_and(|image| Arc::ptr_eq(&image.payload, &payload))
                    {
                        continue;
                    }
                    let mut hash = HashWriter(Sha256::new());
                    payload.write_payload_to(&mut hash)?;
                    self.images.insert(
                        id,
                        Image {
                            payload,
                            address: format!("{:x}", hash.0.finalize()),
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
        for image in self.images.values() {
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
        let image = self.images.get(id)?;
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
