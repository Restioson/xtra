#![feature(async_fn_in_trait)]

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use xtra::prelude::*;

#[derive(xtra::Actor)]
struct Echoer;

struct Echo(String);

impl Handler<Echo> for Echoer {
    type Return = String;

    async fn handle(&mut self, echo: Echo, _ctx: &mut Context<Self>) -> String {
        echo.0
    }
}

#[wasm_bindgen]
pub async fn start() -> Result<(), JsValue> {
    let addr = xtra::spawn_wasm_bindgen(Echoer, Mailbox::unbounded());
    let response = addr
        .send(Echo("hello world".to_string()))
        .await
        .expect("Echoer should not be dropped");

    assert_eq!(response, "hello world");

    Ok(())
}
