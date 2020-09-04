use wasm_bindgen::{prelude::*, JsValue};
use xtra::prelude::*;

struct Echoer;

impl Actor for Echoer {}

struct Echo(String);
impl Message for Echo {
    type Result = String;
}

#[async_trait::async_trait]
impl Handler<Echo> for Echoer {
    async fn handle(&mut self, echo: Echo, _ctx: &mut Context<Self>) -> String {
        echo.0
    }
}

#[wasm_bindgen]
pub async fn start() -> Result<(), JsValue> {
    let addr = Echoer.spawn(None);
    let response = addr
        .send(Echo("hello world".to_string()))
        .await
        .expect("Echoer should not be dropped");

    assert_eq!(response, "hello world");

    Ok(())
}
