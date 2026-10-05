//! 测试 fixture 加载：运行期读取 `testdata/` 下的日志文件。

use std::path::Path;

/// 读取 `testdata/` 下的 SSE fixture（测试用）。
///
/// 运行期读取而非 `include_str!` 嵌入，fixture 变更不需要重编译。路径基于
/// `CARGO_MANIFEST_DIR` 拼出，与测试运行时的 CWD 无关。
pub(crate) fn load(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/provider/responses/testdata")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read test fixture {path:?}: {err}"))
}

pub(crate) fn deepseek_sse() -> String {
    load("deepseek_responses.log")
}

pub(crate) fn grok_sse() -> String {
    load("grok_response.log")
}
