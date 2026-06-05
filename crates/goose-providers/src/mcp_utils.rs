use base64::Engine;
pub use rmcp::model::ErrorData;
use rmcp::model::ResourceContents;

pub type ToolResult<T> = Result<T, ErrorData>;

pub fn extract_text_from_resource(resource: &ResourceContents) -> String {
    match resource {
        ResourceContents::TextResourceContents { text, .. } => text.clone(),
        ResourceContents::BlobResourceContents {
            blob, mime_type, ..
        } => match base64::engine::general_purpose::STANDARD.decode(blob) {
            Ok(bytes) => {
                let byte_len = bytes.len();
                match String::from_utf8(bytes) {
                    Ok(text) => text,
                    Err(_) => {
                        let mime = mime_type
                            .as_ref()
                            .map(|m| m.as_str())
                            .unwrap_or("application/octet-stream");
                        format!("[Binary content ({}) - {} bytes]", mime, byte_len)
                    }
                }
            }
            Err(_) => blob.clone(),
        },
    }
}
