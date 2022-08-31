#[derive(xtra::Actor)]
struct MyActor<A, B> {
    phantom: std::marker::PhantomData<(A, B)>
}

#[derive(xtra::Actor)]
struct MyActorWithBounds<A: Bar, B> {
    phantom: std::marker::PhantomData<(A, B)>
}

#[derive(xtra::Actor)]
struct MyActorWithWhere<A, B> where A: Bar {
    phantom: std::marker::PhantomData<(A, B)>
}

struct Foo;

trait Bar { }

impl Bar for &'static str {

}

fn main() {
    macros_test::assert_actor::<MyActor<&'static str, Foo>>();
    macros_test::assert_actor::<MyActorWithBounds<&'static str, Foo>>();
    macros_test::assert_actor::<MyActorWithWhere<&'static str, Foo>>();
}
