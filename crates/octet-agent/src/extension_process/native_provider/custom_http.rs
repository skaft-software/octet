use super::*;

pub(super) struct ProcessHttpSender {
    pub(super) connection: Weak<ProcessConnection>,
    pub(super) stream_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseHead {
    status: u16,
    headers: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BodyChunk {
    Chunk(EncodedChunk),
    End(EndChunk),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EncodedChunk {
    chunk: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EndChunk {
    done: bool,
}

fn decode_chunk(value: serde_json::Value) -> Result<Option<bytes::Bytes>, AiError> {
    match serde_json::from_value(value).map_err(|_| provider_unavailable_error())? {
        BodyChunk::Chunk(chunk) => {
            if chunk.chunk.is_empty() || chunk.chunk.len() > 87384 {
                return Err(provider_unavailable_error());
            }
            let data = base64::engine::general_purpose::STANDARD
                .decode(chunk.chunk)
                .map_err(|_| provider_unavailable_error())?;
            if data.is_empty() || data.len() > 65536 {
                return Err(provider_unavailable_error());
            }
            Ok(Some(data.into()))
        }
        BodyChunk::End(EndChunk { done: true }) => Ok(None),
        BodyChunk::End(_) => Err(provider_unavailable_error()),
    }
}

#[async_trait::async_trait]
impl octet_ai::HostHttpSender for ProcessHttpSender {
    async fn send(
        &self,
        request: octet_ai::HostHttpRequest,
    ) -> Result<octet_ai::HostHttpResponse, AiError> {
        let connection = self
            .connection
            .upgrade()
            .ok_or_else(provider_unavailable_error)?;
        if !connection_is_usable(&connection) || request.body.len() > 786432 {
            return Err(provider_unavailable_error());
        }
        let headers = request
            .headers
            .iter()
            .map(|(key, value)| {
                value
                    .to_str()
                    .map(|value| (key.as_str().to_owned(), value.to_owned()))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(|_| provider_unavailable_error())?;
        let head = connection.request(
            "provider/delegate_http",
            serde_json::json!({
                "stream_id": self.stream_id,
                "method": request.method.as_str(),
                "url": request.url.as_str(),
                "headers": headers,
                "body": std::str::from_utf8(&request.body).map_err(|_| provider_unavailable_error())?,
            }),
            Duration::from_secs(5),
        ).await.map_err(|_| provider_unavailable_error())?;
        let head: ResponseHead =
            serde_json::from_value(head).map_err(|_| provider_unavailable_error())?;
        if !(100..=599).contains(&head.status)
            || head.headers.len() > 128
            || head
                .headers
                .iter()
                .map(|(key, value)| key.len() + value.len())
                .sum::<usize>()
                > 32768
        {
            return Err(provider_unavailable_error());
        }
        let weak = self.connection.clone();
        let stream_id = self.stream_id.clone();
        let body: std::pin::Pin<
            Box<dyn futures_core::Stream<Item = Result<bytes::Bytes, AiError>> + Send>,
        > = Box::pin(async_stream::try_stream! {
            loop {
                let connection = weak.upgrade().ok_or_else(provider_unavailable_error)?;
                if !connection_is_usable(&connection) {
                    Err(provider_unavailable_error())?;
                }
                let value = connection.request(
                    "provider/delegate_http_chunk",
                    serde_json::json!({"stream_id": stream_id}),
                    Duration::from_secs(5),
                ).await.map_err(|_| provider_unavailable_error())?;
                let Some(chunk) = decode_chunk(value)? else { break; };
                yield chunk;
            }
        });
        let mut response = octet_ai::HostHttpResponse::new(reqwest::Body::wrap_stream(body));
        *response.status_mut() =
            reqwest::StatusCode::from_u16(head.status).map_err(|_| provider_unavailable_error())?;
        for (key, value) in head.headers {
            response.headers_mut().insert(
                reqwest::header::HeaderName::from_bytes(key.as_bytes())
                    .map_err(|_| provider_unavailable_error())?,
                reqwest::header::HeaderValue::from_str(&value)
                    .map_err(|_| provider_unavailable_error())?,
            );
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_chunks_are_exact_bounded_and_terminal() {
        let data = vec![42; 65536];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
        assert_eq!(
            decode_chunk(serde_json::json!({"chunk": encoded}))
                .unwrap()
                .unwrap()
                .as_ref(),
            data
        );
        assert!(decode_chunk(serde_json::json!({"done": true}))
            .unwrap()
            .is_none());
        for value in [
            serde_json::json!({"chunk": ""}),
            serde_json::json!({"chunk": "%%%"}),
            serde_json::json!({"chunk": base64::engine::general_purpose::STANDARD.encode(vec![0; 65537])}),
            serde_json::json!({"done": false}),
            serde_json::json!({"done": true, "chunk": "YQ=="}),
            serde_json::json!({"chunk": "YQ==", "extra": true}),
        ] {
            assert!(decode_chunk(value).is_err());
        }
    }
}
