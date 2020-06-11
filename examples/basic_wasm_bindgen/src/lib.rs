use wasm_bindgen::{prelude::*, JsValue};
use xtra::prelude::*;

struct Printer;

impl Printer {
    fn new() -> Self {
        Printer {}
    }
}

impl Actor for Printer {}

struct Print(String);
impl Message for Print {
    type Result = String;
}

impl SyncHandler<Print> for Printer {
    fn handle(&mut self, print: Print, _ctx: &mut Context<Self>) -> String {
        print.0
    }
}

#[wasm_bindgen]
pub async fn start() -> Result<(), JsValue> {
    let addr = Printer::new().spawn();
    let response = addr
        .send(Print("hello world".to_string()))
        .await
        .expect("Printer should not be dropped");

    assert_eq!(response, "hello world");

    Ok(())
}
