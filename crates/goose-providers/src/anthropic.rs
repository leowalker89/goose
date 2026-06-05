use crate::base::{Error, Provider, StreamingRequest, StreamingResponse};

pub struct Anthropic;

impl Provider for Anthropic {
    async fn stream(_req: StreamingRequest) -> Result<StreamingResponse, Error> {
        todo!()
    }
}
