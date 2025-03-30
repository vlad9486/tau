use super::driver::Runtime;

pub async fn run(rt: &Runtime) {
    let _ = rt.wait().await;
}
