#[derive(xtra::Actor)]
struct MyActor;

fn main() {
    macros_test::assert_actor::<MyActor>()
}
