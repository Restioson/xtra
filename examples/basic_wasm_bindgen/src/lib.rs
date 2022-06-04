use wasm_bindgen::{prelude::*, JsValue};

use xtra::prelude::*;
use xtra::spawn::WasmBindgen;

struct Echoer;

#[async_trait]
impl Actor for Echoer {
    type Stop = ();

    async fn stopped(self) {}
}

struct Echo(String);

#[async_trait]
impl Handler<Echo> for Echoer {
    type Return = String;

    async fn handle(&mut self, echo: Echo, _ctx: &mut Context<Self>) -> String {
        echo.0
    }
}

#[wasm_bindgen]
pub async fn start() -> Result<(), JsValue> {
    let addr = Echoer.create(None).spawn(&mut WasmBindgen);
    let response = addr
        .send(Echo("hello world".to_string()))
        .await
        .expect("Echoer should not be dropped");

    assert_eq!(response, "hello world");

    Ok(())
}
