//! Typed user input crossing the agent boundary: ordered text and media parts.

use octet_ai::{Media, UserPart};

/// A user-authored input: ordered text and media parts.
///
/// This is the type accepted by [`Agent::prompt`](crate::Agent::prompt),
/// [`Agent::prompt_without_tools`](crate::Agent::prompt_without_tools),
/// [`RunControl::steer`](crate::RunControl::steer),
/// [`RunControl::finish_now`](crate::RunControl::finish_now), and
/// [`RunControl::follow_up`](crate::RunControl::follow_up). Plain strings
/// convert via `From`, so text-only callers pass `&str`/`String` unchanged.
#[derive(Clone, Debug)]
pub struct UserInput {
    /// Ordered content parts.
    pub parts: Vec<InputPart>,
    /// Custom messages appended as independent entries after the user prompt.
    pub custom_messages: Vec<crate::session::CustomMessage>,
}

/// One part of a [`UserInput`].
#[derive(Clone, Debug)]
pub enum InputPart {
    /// Plain text.
    Text(String),
    /// Image or audio payload.
    Media(Media),
}

impl From<String> for UserInput {
    fn from(text: String) -> Self {
        Self {
            parts: vec![InputPart::Text(text)],
            custom_messages: Vec::new(),
        }
    }
}

impl From<&str> for UserInput {
    fn from(text: &str) -> Self {
        Self::from(text.to_owned())
    }
}

impl From<Vec<InputPart>> for UserInput {
    fn from(parts: Vec<InputPart>) -> Self {
        Self {
            parts,
            custom_messages: Vec::new(),
        }
    }
}

impl UserInput {
    /// A custom-only prompt, without a substitute user message.
    pub fn from_custom(message: crate::session::CustomMessage) -> Self {
        Self {
            parts: Vec::new(),
            custom_messages: vec![message],
        }
    }

    /// Persist the prompt and each custom message independently, in Pi order.
    /// Returns the first entry, which is also the run's checkpoint prompt.
    pub fn append_to(
        self,
        session: &mut crate::session::Session,
        metadata: Option<crate::session::EntryMetadata>,
    ) -> Result<crate::session::EntryId, crate::session::SessionError> {
        for message in &self.custom_messages {
            message.validate()?;
        }
        let mut first = None;
        if !self.parts.is_empty() || self.custom_messages.is_empty() {
            let message = octet_ai::Message::User(octet_ai::UserMessage {
                content: UserInput::from(self.parts).into_user_parts(),
            });
            first = Some(session.append_with_metadata(
                crate::session::EntryValue::Message(message),
                metadata.clone(),
            )?);
        }
        for message in self.custom_messages {
            let id = session.append_custom_message(message, metadata.clone())?;
            if first.is_none() {
                first = Some(id);
            }
        }
        Ok(first.expect("input always appends at least one entry"))
    }

    /// Human-readable single-line summary: text parts joined, media parts as
    /// `[image]` / `[audio]`. Used for steering-delivery events and logs.
    pub fn text_summary(&self) -> String {
        let mut pieces = Vec::with_capacity(self.parts.len());
        for part in &self.parts {
            match part {
                InputPart::Text(text) => pieces.push(text.clone()),
                InputPart::Media(Media::Image(_)) => pieces.push("[image]".into()),
                InputPart::Media(Media::Audio(_)) => pieces.push("[audio]".into()),
            }
        }
        pieces.extend(self.custom_messages.iter().map(|message| message.text()));
        pieces.join(" ")
    }

    /// Human-visible delivery summary, excluding hidden custom messages.
    pub fn display_summary(&self) -> String {
        let ordinary = UserInput::from(self.parts.clone()).text_summary();
        let mut pieces = Vec::new();
        if !ordinary.is_empty() {
            pieces.push(ordinary);
        }
        pieces.extend(
            self.custom_messages
                .iter()
                .filter(|message| message.display)
                .map(|message| format!("[{}]\n{}", message.custom_type, message.text())),
        );
        pieces.join("\n")
    }

    /// Canonical projection of ordinary and custom input content.
    /// Persist with `append_to` to retain independent custom entry identities.
    pub fn into_user_parts(self) -> Vec<UserPart> {
        self.parts
            .into_iter()
            .map(|part| match part {
                InputPart::Text(text) => UserPart::Text(text),
                InputPart::Media(media) => UserPart::Media(media),
            })
            .chain(
                self.custom_messages
                    .iter()
                    .flat_map(|message| message.user_parts()),
            )
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_media() -> Media {
        Media::image_bytes(
            bytes::Bytes::from_static(&[0x89, 0x50]),
            "image/png".parse().unwrap(),
        )
    }

    #[test]
    fn from_string_yields_one_text_part() {
        let input = UserInput::from("hello".to_owned());
        assert!(matches!(&input.parts[..], [InputPart::Text(t)] if t == "hello"));
    }

    #[test]
    fn text_summary_joins_text_and_labels_media() {
        let input = UserInput::from(vec![
            InputPart::Text("look at".into()),
            InputPart::Media(png_media()),
            InputPart::Text("please".into()),
        ]);
        assert_eq!(input.text_summary(), "look at [image] please");
    }

    #[test]
    fn into_user_parts_maps_one_to_one_preserving_order() {
        let input = UserInput::from(vec![
            InputPart::Text("a".into()),
            InputPart::Media(png_media()),
        ]);
        let parts = input.into_user_parts();
        assert_eq!(parts.len(), 2);
        assert!(matches!(&parts[0], UserPart::Text(t) if t == "a"));
        assert!(matches!(&parts[1], UserPart::Media(Media::Image(_))));
    }
}
