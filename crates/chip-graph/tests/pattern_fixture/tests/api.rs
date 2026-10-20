use svc::ops::create_user;

#[tokio::test]
async fn creates_a_user() {
    create_user(1).unwrap();
}

#[tokio::test]
async fn rejects_nothing() {
    let _ = 1;
}

#[tokio::test]
async fn third_async() {}

#[test]
fn a_plain_test() {}
