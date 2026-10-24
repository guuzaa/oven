# oven-llm 重新设计：借鉴 genai 的抽象，不引入依赖

> 状态：设计提案（讨论稿）
> 关联：[`pro.md`](./pro.md) —— 思考强度现状调查与增量方案
> 参考：genai 0.7.1（MIT OR Apache-2.0）的抽象思路，源码链接见文末

---

## 0. 结论

1. **借鉴 genai 的抽象，不引入 genai 依赖。** oven 自己实现 wire 编码，但把 genai 的关键抽象抄过来：单轴推理词表、vendor 自持的能力推导、映射与降级收敛在 vendor 层、请求与可调项分离。
2. **降级采用 genai 的策略：请求路径静默、只降不报错、不对外暴露。** 请求路径不记录、不上报降级。
3. **切换时给一次 warning。** 这是用户唯一能拿到反馈的地方：选到模型不支持的档位时提示"将按 X 处理"；配置值保持用户写下的原样。
4. **推理词表不保留"开但不定档"这一档。** 开关型模型上，任意档位都表达"开启"（与 genai 的取舍一致）。
5. **能力表仍然要存在**，用途三件事：钳制（决定发什么）、picker 选项来源、UI 按能力渲染语义。

---

## 1. 现状诊断：三个正交的东西被塞进了同一个扁平结构

`Request` 上挂着两个互不关联的字段：

```rust
// crates/oven-llm/src/domain/request.rs:180-181
pub thinking: Option<ThinkingMode>,
pub reasoning_effort: Option<ReasoningEffort>,
```

它们同时扮演三个角色：

| 角色 | 含义 | 今天的落点 |
| --- | --- | --- |
| **意图** | 用户要什么（不表态 / 关 / 某个强度） | `Request` 的两个字段 |
| **能力** | 模型支持什么（GLM 没档位、deepseek 没 medium） | `ModelCapabilities`（`provider/model.rs:130-138`，无推理字段） |
| **方言** | wire 上怎么写（`thinking.type` / `reasoning_effort` / `reasoning.effort`） | encoder 内部（`completions/encoder.rs:81-85` 透传、`responses/encoder.rs:340-351` 三分支覆盖） |

三者纠缠的必然后果：

- `Option::None` 在四层被重载成两种意思（`pro.md` 2.4 的 bug 根因）；
- encoder 只好自己补优先级规则（Responses 侧 effort 覆盖 thinking）；
- 调用方只好替协议层做决定（`oven-agent/src/runtime/agent/request.rs:62-66` 用 effort 反推 thinking）；
- UI 只好硬编码档位常量（`oven-tui/src/widgets/model_picker.rs:14-20`）。

> 一句话概括：**协议层在做能力判断，能力层不在协议层，上游层在替协议层做决定。**

---

## 2. 从 genai 借鉴的五件事

### 2.1 域层只有一根轴，且显式区分"不表态"与"关闭"

```rust
// genai 0.7.1: src/chat/chat_options.rs
pub enum ReasoningEffort {
    Zero, Low, Medium, High, XHigh, Max,
    Budget(u32),   // 厂商按 token 预算表达
    Minimal,       // legacy
}
```

它的文档注释是关键：

> `Zero` — "Explicitly request no reasoning. **This differs from leaving
> `ChatOptions::reasoning_effort` unset, which expresses no preference.**"

`Option::None`（不表态）与 `Some(Zero)`（显式关闭）在**类型**上就是两件事，而不是靠注释约定。这正是 `pro.md` 想解决却没解决的语义。

`Budget(u32)` 把 Anthropic 风格的 token 预算提升为一等公民，而不是塞进 `provider_options` 这种字符串逃生口。

注意 genai 的词表里也**没有"开启但不指定档位"这一档**：开关型厂商（Anthropic 的 `thinking.type`、GLM 的 `thinking.type`）把"开启"表达成"一个档位请求"，于是开关型模型上任意档位都等于"开启"。oven 采纳同一取舍。

### 2.2 能力由 vendor 自己推导，不集中在一张谁也维护不动的表里

```rust
// genai 0.7.1: src/adapter/adapters/anthropic/ant_model.rs
pub(super) struct AnthropicModelCapabilities {
    pub supports_effort: bool,
    pub supports_max_effort: bool,
    pub supports_xhigh_effort: bool,
    pub supports_adaptive_thinking: bool,
    pub thinking_enabled_by_default: bool,   // ← 直接解决 "unset ≠ off"
    pub supports_legacy_budget_thinking: bool,
    pub max_tokens: AnthropicMaxTokens,
}
```

`AnthropicModel::parse(name)` 解析 family + version 后算出这组能力 —— **能力推导跟着厂商走，而不是集中在一张目录表里**。

### 2.3 映射与降级收敛在 vendor 层，只降不报错

```rust
// genai 0.7.1: src/adapter/adapters/anthropic/ant_reasoning.rs
// Zero → 只有 thinking_enabled_by_default 才显式发 thinking:{type:"disabled"}，否则省略即关闭
// Minimal → "low"；XHigh 不支持 → "high"；Max 不支持 → "high"
// Budget(n) → 保留预算，映射到 adaptive thinking 或 legacy budget_tokens
```

降级是**局部且隐式**的：vendor 模块自己知道本厂商支持什么，于是直接产出该厂商能接受的词。

### 2.4 降级不对外暴露（本次明确采纳）

genai 的降级结果**不对调用方暴露**：`XHigh` 被降成 `high` 之后，调用方无从得知。

**oven 采纳同一策略**：

- 请求路径不记录降级原因、不上报、不产生噪音日志；
- 请求路径永不因档位问题报错；
- 用户能拿到的反馈只有一处：**切换时的一次 warning**（§7.3）。

### 2.5 请求与可调项分离，且默认值有级联

```rust
// genai 0.7.1: src/chat/chat_options.rs
pub(crate) struct ChatOptionsSet<'a, 'b> { client: Option<&'a ChatOptions>, chat: Option<&'b ChatOptions> }
```

"客户端默认 ← 单次覆盖"这一条级联规则只写一次，adapter 通过 `ChatOptionsSet` 取值，不需要各自实现优先级。

### 2.6 录放式回归测试（yakbak）

genai 用录制的真实 provider 响应做回放测试（`tests/data/yakbak/<provider>/<case>/response_000.txt`），对 wire body 与解码结果同时断言。

---

## 3. 为什么不直接依赖 genai（决策记录）

读的是 v0.7.1 源码，三条都是实测确认的：

1. **GLM 的方言没覆盖**。`ZaiAdapter::to_web_request_data` 走 `OpenAIAdapter::util_to_web_request_data(..., None)` —— 只发 OpenAI 风格的 `reasoning_effort`，**不发 GLM 原生的 `thinking.type`**。oven 现在的双字段广播对 GLM 反而是超集。
2. **流式协议不对等**。genai 是 `Chunk`（纯文本）/ `ReasoningChunk` / `ToolCallChunk`（参数是完整 JSON），没有 oven 的块索引协议（`ContentBlockStart/Delta/End` + `InputJsonDelta`）。
3. **没有模型元数据**。`Adapter::all_model_names` 只返回 `Vec<String>` —— context window / max output / pricing / 能力位都没有，oven 的 compact 阈值、成本显示、picker 过滤无处可取。

外加 0.x 版本的破坏性小版本频繁（仓库自带 `migration-v_0_6_to_0_7.md`）。

结论：**抽象借鉴，依赖不引入。**

---

## 4. 目标架构

```text
crates/oven-llm/
  domain/
    request.rs      ← 改：Request 拆成 Request + CallOptions
    reasoning.rs    ← 新增：单轴推理词表
    message.rs / response.rs / stream.rs / tool.rs   ← 不动
  provider/
    vendor/
      mod.rs        ← 新增：VendorProfile（协议 + 方言）
      capability.rs ← 新增：EffortSupport 与档位钳制
      openai.rs     ← 新增：OpenAI 系的映射与能力
      glm.rs        ← 新增：GLM 系的映射与能力
      deepseek.rs / moonshot.rs / xai.rs ← 新增：各家映射与能力
    codec/
      completions/  ← 保留：wire 编解码（types / encoder / decoder）
      responses/    ← 保留：wire 编解码
    model.rs        ← 保留：ModelInfo / ModelCapabilities（产品数据）
    catalog.rs      ← 保留：静态目录（窗口 / 价格 / 协议）
    router.rs       ← 保留：按模型派发到 vendor
    validate.rs     ← 保留：请求前静态校验
    builder.rs      ← 改：按 vendor 装配 profile
```

依赖方向不变（`oven-tui → oven-app → oven-agent → oven-llm`），`oven-proto` 仍是叶子 crate。

---

## 5. 领域层

### 5.1 单轴词表

```rust
// crates/oven-llm/src/domain/reasoning.rs

/// 推理强度意图。`Option::None` = 不表态（不下发，交给上游默认）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    /// 显式关闭。与"不表态"是两件事。
    #[serde(rename = "none", alias = "off", alias = "zero")]
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
    /// 厂商按 token 预算表达强度。
    Budget(u32),
}
```

对比 `pro.md` 的 `Reasoning { effort: Option<..>, thinking: Option<..> }`：**那只是把两个 `Option` 装进一个 struct，旧的歧义原封不动搬了家**。用"一个枚举 + `Option` 包裹"，"不表态 / 关 / 档位 / 预算"四种状态全部由类型保证，非法组合不可表示。

阶梯顺序由**声明顺序**表达；`Off` / `Budget` 不在阶梯上，因此不需要 `pro.md` 3.1 那个返回 `Option<u8>` 的 `rank()`（每个 match 分支都要单独处理 `None`）。

**没有"开启但不指定档位"这一档**：开关型模型上，任意档位都表达"开启"（§6.4）。用户想要"开着但不指定强度"时，在开关模型上写任意档位即可，语义上没有损失。

### 5.2 请求与可调项分离

```rust
pub struct Request {
    pub model: ModelId,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<Tool>,
    pub tool_choice: ToolChoice,
    pub options: CallOptions,
}

pub struct CallOptions {
    pub sampling: SamplingParams,
    /// None = 不表态
    pub reasoning_effort: Option<ReasoningEffort>,
    /// 真正私有的逃生口
    pub provider_options: serde_json::Map<String, serde_json::Value>,
}
```

好处：app 层可以把 config 里的默认选项构建一次，agent 只在需要时覆盖；`compact` 这种"临时关思考"不再需要改共享状态。

---

## 6. 能力与方言

### 6.1 两层分开

```rust
// provider/vendor/capability.rs —— 模型级：支持哪些档位
pub enum EffortSupport {
    /// 不知道：不干预
    Unknown,
    /// 完全不支持推理：不发任何推理字段
    Unsupported,
    /// 只有开关，没有档位：任意档位都表达"开启"，`Off` 表达"关闭"
    Toggle,
    /// 有档位：这些档位可用（升序、去重、非空）。不含 `Off` 表示不支持显式关闭
    Levels(Vec<ReasoningEffort>),
}

// provider/vendor/mod.rs —— 厂商×协议级：wire 上用哪条通道说话
pub enum Dialect {
    /// 档位通道：`reasoning_effort` / `reasoning.effort`
    Level,
    /// 开关通道：`thinking.type`（GLM 类）
    Toggle,
    /// 两条都发（自定义 / 代理，兜底）
    Both,
}

pub struct VendorProfile {
    /// wire 协议
    pub protocol: Protocol,
    /// 推理方言
    pub dialect: Dialect,
}
```

**为什么要分开**：GLM 与 OpenAI 的差异不是"模型能力未知"，而是"厂商方言不同" —— 这个信息静态、可知。`pro.md` 2.3 的三个失效场景，有两个是方言问题：

| 场景 | 方言决定 | 能力决定 |
| --- | --- | --- |
| GLM 上选 high | 只发 `thinking.type`，不发 `reasoning_effort` | high → 开启（静默） |
| 老 OpenAI chat 上选 none | 发 `reasoning_effort` | none → low（静默） |
| 不支持推理的模型上选 high | — | 不下发任何推理字段 |

### 6.2 能力的来源：vendor 默认 + 模型例外 +（可选）名字推导

```rust
// provider/vendor/glm.rs
/// GLM 系：全部只有开关，没有档位。
pub const CAPABILITIES: EffortSupport = EffortSupport::Toggle;
```

| vendor | 默认能力 | 例外 |
| --- | --- | --- |
| `openai` | `Levels([minimal, low, medium, high, xhigh])` | 老模型子集 |
| `glm` | `Toggle` | 个别模型支持档位时覆盖 |
| `deepseek` | `Levels([low, medium, high])` | — |
| `moonshot` | `Levels([low, medium, high, xhigh, max])` | `kimi-k2.6` → `Unsupported` |
| `xai` | `Levels([low, medium, high])` | — |
| `custom` | `Unknown` | — |

三条覆盖优先级（高 → 低）：**用户 config 声明 > 模型例外 > vendor 默认**。

genai 的"从模型名推导"作为可选增强：当版本号能决定能力时（例如 `glm-5.1` 与 `glm-5.3` 的档位不同），用解析式推导而不是逐条列举。oven 的模型名（`gpt-5.6-terra`、`deepseek-v4-flash`）目前还不满足这个条件，先不做。

### 6.3 归一化规则

钳制规则（档位集合、向下取档）是**公共**的，放在 `vendor/capability.rs`；"关闭/开启"怎么写进 wire 是**方言**相关的，放在各 vendor 模块。两边合起来构成一条规则：

| 意图 \ 能力 | Unknown | Unsupported | Toggle | Levels(S) |
| --- | --- | --- | --- | --- |
| None（不表态） | 不发 | 不发 | 不发（= 上游默认） | 不发 |
| Off | 发"关闭" | 不发 | 发"关闭" | S 含 Off → 发"关闭"，否则降到 S 最低档 |
| Level(L) | 发 L | 不发 | 发"开启"（忽略 L） | 钳到 S 中 rank ≤ L 的最高档，都更强则 S[0] |
| Budget(n) | 发预算 | 不发 | 发"开启" | 落到 S[0] |

三条不变量：

- **`None` 在任何能力下都是"不发"** —— 不表态就永远不下发。这是 `pro.md` 2.4 那个 bug 的根治点。
- **只向下，永不报错**。用户能操作的拒绝点只剩配置加载（拼错 → 加载即报）与 picker（只给模型支持的档位）。
- **降级不外露**（§2.4）：规则执行完就结束，不产出"降级原因"。

### 6.4 vendor 映射

```rust
// provider/vendor/glm.rs
/// GLM：只有 `thinking.type` 开关，没有档位。
pub fn encode_reasoning(body: &mut serde_json::Map<String, Value>, effort: Option<ReasoningEffort>) {
    let Some(effort) = effort else { return };              // 不表态：不下发
    let on = !matches!(effort, ReasoningEffort::Off);
    body.insert("thinking".into(), json!({"type": if on { "enabled" } else { "disabled" }}));
}

// provider/vendor/openai.rs
/// OpenAI 系：档位走 `reasoning_effort`。
pub fn encode_reasoning(body: &mut Map<String, Value>, effort: Option<ReasoningEffort>) {
    let Some(effort) = effort else { return };
    let value = match effort {
        ReasoningEffort::Off => "none",
        ReasoningEffort::Budget(_) => return, // 不支持预算：省略
        other => other.as_str(),
    };
    body.insert("reasoning_effort".into(), json!(value));
}
```

encoder 只做"把意图写进 body"，**零策略**；策略全部在 vendor 模块里。开关型模型上"开启"不需要单独的意图值 —— 任意档位都映射成 `thinking.type = enabled`。

---

## 7. 展示与反馈：选项来自能力，切换时给一次 warning

请求路径静默（§2.4），所以反馈要放在用户**能操作**的地方：选择与切换。

### 7.1 picker 选项来自能力

选中模型后，档位列表由该模型的 `EffortSupport` 决定：

| 能力 | picker 选项 |
| --- | --- |
| `Levels(S)` | `S` 的各档位；模型支持关闭时加「关闭」 |
| `Toggle` | 「默认」与「关闭」 |
| `Unsupported` | 只有「关闭 / 不表态」 |
| `Unknown` | 全部档位（不干预） |

`pro.md` 2.1 的硬编码 `EFFORT_ITEMS` 随之消失。

### 7.2 渲染按能力，不按原始值

同一个值在不同能力下语义不同，渲染必须带能力：

| 能力 | 值 | 显示 |
| --- | --- | --- |
| `Toggle` | 任意非 `Off` | 开启 |
| `Toggle` | `Off` | 关闭 |
| `Toggle` | `None` | 默认 |
| `Levels(S)` | 在 `S` 内 | 该档位名 |
| `Levels(S)` | 不在 `S` 内 | 按钳制结果渲染（与切换时的 warning 一致） |
| `Unsupported` | 任意 | 不支持 |
| `Unknown` | 任意 | 原样 |

状态栏与 `/model` 用同一个渲染函数；`describe` 今天把 unset 打印成 `none`（`pro.md` 1.8），改为"未指定"，两边口径一致。

### 7.3 切换时给一次 warning

用户切换模型或档位时，若请求值超出该模型的能力，给一条 warning 并说明实际会怎么处理：

```
! glm-5.3 只支持开关，high 将按「开启」处理
! deepseek-v4-flash 最高支持 high，xhigh 将按 high 处理
```

- 发生在**切换时**（`/model`、profile 切换、启动时按配置加载），**不在请求路径**；
- **配置值保持用户写下的原样**（配置 = 用户的心愿），实际发什么由请求路径按能力静默决定；
- 切换后的状态栏按 §7.2 渲染，因此显示与实际一致。

于是"用户以为生效了但其实没生效"分成两类：

- **用户能操作的**（picker / `/model`）→ 选项来自能力，且切换时有 warning；
- **用户手写 config 的** → 加载时 warning 一次，之后静默。

---

## 8. 可信度：靠测试，不靠显示

请求路径不记录降级，就要用别的方式保证"发出去的东西是对的"：

1. **录放式回归测试**（yakbak 风格）：录一份真实 provider 响应（`tests/data/yakbak/<vendor>/<case>/`），回放并断言 **wire body + 解码结果**。这能锁死"某个模型 + 某个档位 → 发出去的字段"。
2. **矩阵测试**：`意图 × 能力 × vendor` 表驱动（4 × 4 × 6），断言 wire body。
3. **性质测试**（仓库已有 `proptest` 依赖）：不变量两条 —— `None` 恒为"不发"；**降级后的值一定在该模型的 `Levels` 内**（或落在 `Toggle` 的"开启"上）。
4. **能力校准**：按 (model, effort) 矩阵实测 `Usage.reasoning_tokens`，把结论写回 §6.2 的例外表。`reasoning_tokens` 是唯一能证明"档位真的生效了"的信号。

---

## 9. 顺带修掉的相邻问题

与推理强度无关，但同属"模型身份与路由"这一团：

1. **协议选择三处真源**：`ModelId::variant()`（`router.rs:171-183`）、`ProviderName::default_protocol()`（`provider/mod.rs:167`）、`ModelInfo.protocols`（`model.rs:19`）。建议：`ModelId` 只做身份（vendor + wire id），协议选择统一收口到 `Router`（按 `ModelInfo.protocols` + 显式 override），删掉 `:responses` 变体语法。
2. **会撒谎的能力位**：`ModelCapabilities.supports_parallel_tool_calls` 被 catalog 赋值（`catalog.rs:95`），但 `validate_request` 明确不校验它（`validate.rs:36-39`）。从不影响行为的字段就是噪声，删掉或挪到 runtime 行为区。
3. **`provider_options` 的冲突语义**：现在是"最后 merge 覆盖一切"（`endpoint.rs:69-83`），与标准字段重名时静默覆盖。改成：标准字段优先，`provider_options` 命中重名则跳过。加了 `Budget(u32)` 之后，逃生口只需承载真正的私有字段。
4. **Responses 丢弃思维块**（`responses/encoder.rs:263`）：Completions 侧会把 `Thinking` 合并进 `reasoning_content` 回传，Responses 侧直接丢。这会破坏多轮推理轨迹的一致性。"思维内容是否回传"应提升到领域层决定，而不是由 encoder 各自决定。

---

## 10. 风险

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| 请求路径完全静默，手写 config 写错档位无声无息 | 用户以为生效了 | 切换/加载时 warning（§7.3）；picker 选项来自能力，用户操作路径不会写到非法值 |
| 能力表仍是手工维护 | 半年后失准 | 厂商默认覆盖大多数情况，目录只记例外；用 `reasoning_tokens` 实测校准 |
| 开关型模型上"开启"由任意档位表达 | 用户困惑"我写的 low 怎么变成开启了" | 渲染层按能力显示「开启」；warning 里说明"只支持开关" |
| vendor 模块数量增长 | 维护面变大 | 共享 `codec/`，vendor 模块只写差异部分（映射 + 能力），不重复 wire 编解码 |
| 协议仍只有两种 | Anthropic 仍缺 | 未来加 `codec/messages` + `vendor/anthropic.rs`，不影响其余结构 |

---

## 11. 落地顺序

每一步独立可发：

1. **词表与领域模型**：`ReasoningEffort` 扩成 `Off/Minimal..Max/Budget`（无 `On`）；`Request` 拆出 `CallOptions`；老 TOML（`"none"`）仍能读。验收：`Option::None` 与 `Some(Off)` 在类型上不可混淆。
2. **能力与方言**：新增 `EffortSupport` / `Dialect` / `VendorProfile` 与 vendor 模块骨架；`ModelInfo.effort_support` 作为例外覆盖的落点。
3. **归一化与映射**：公共钳制 + 各 vendor 的 `encode_reasoning()`；两个 codec 接线；encoder 变成纯映射。验收：§8 的矩阵测试 + 三条不变量。
4. **展示与反馈**：picker 按能力生成档位；状态栏与 `/model` 按能力渲染；切换与加载时给 warning；修掉 unset 显示成 `none` 的不一致。
5. **录放测试设施**：建 `tests/data/yakbak/`，先录 6 个 vendor 的基线响应。
6. **能力校准**：probe（model × effort 矩阵）读 `reasoning_tokens`，结论写回例外表。
7. **profile 层**（在 oven-app）：`[profiles.<name>.<provider>]` → `(model, effort)`；手工 `/model` 即解绑 profile。

---

## 12. 与 pro.md 的差异

| 维度 | pro.md | 本文 |
| --- | --- | --- |
| 域层类型 | `Reasoning { effort: Option, thinking: Option }` | `Option<ReasoningEffort>`，枚举含 `Off/Budget` |
| 厂商差异在哪 | 能力模型（`Unknown` 时广播） | **方言**（厂商×协议）+ 能力模型（模型） |
| GLM 选 high | 能力已知才收窄，`Unknown` 仍广播 | 方言决定：只发 `thinking.type`，不发 `reasoning_effort` |
| 档位预算 | 无，走 `provider_options` | `Budget(u32)` 一等公民 |
| 词表是否含"开但不定档" | 含 `On` | **不含**：开关型模型上任意档位 = 开启（genai 同款取舍） |
| 降级 | 记录 `Adjustment`，上报并展示有效值 | **请求路径静默，不暴露**（genai 策略） |
| 有效值展示 | 要求状态栏与 `/model` 显示有效值 | 请求路径不记录降级；UI 按能力渲染语义 |
| 能力表用途 | 展示 + 钳制 + 早期拒绝 | 钳制 + picker 选项来源 + UI 渲染依据（**不为展示单独维护**） |
| 切换时行为 | 不可达档位直接拒绝 | **warning 告知将按什么处理**，配置值不改写 |
| 目录定位 | 唯一真源 | vendor 默认 + 模型例外；目录只记例外 |
| 解析输出 | 改写 `Request` → 需要 `Cow` | 独立返回值 → **零 clone** |

---

## 13. 待决策

1. **能力例外表的初始内容**。需要查 6 家厂商的文档确定默认值（尤其 DeepSeek / Moonshot 的档位集合与是否支持 `none`）。
2. **开关型模型的默认行为**。当前假设"开关型模型默认开启推理"（GLM 类事实），因此「默认」= 开启；若某模型默认关闭，需要在能力表里显式区分，并决定 picker 是否提供「显式开启」项（届时需要一个可承载"开启"的档位值）。
3. **warning 的落地位置**：`/model` 切换、profile 切换、启动加载三处是否统一用同一个提示函数，以及 warning 文案里的方言表述。
4. **profile 与 tier 的命名**。建议统一叫 profile，避免与 effort 档位撞名。

---

## 附：落地记录（2026-10-24，branch `refactor/oven-llm`）

前四批已经落地，与本文的三处偏差记在这里：

1. **厂商只声明方言，wire 的拼写归 codec。** 本文原写"每 vendor 一个
   `encode_reasoning()`"，实际落地是：vendor 给出 `Dialect`（`Level` /
   `Toggle` / `Both`）与 `EffortSupport`，codec（Completions / Responses）负责把
   通道写进各自的 wire 形状。否则 6 个 vendor × 2 个协议 = 12 份几乎一样的映射。
2. **`EffortSupport::Levels` 用 `Cow<'static, [ReasoningEffort]>`。** 厂商默认值是
   静态切片（不分配），配置声明的档位将来可以是 owned。
3. **开关型模型的"开启"没有独立的值。** 领域词表按决定不保留 `On`：开关型模型上
   任意档位都表达"开启"，picker 用最低档承载这一项。

各批的落点：

| 批次 | 提交 | 落点 |
| --- | --- | --- |
| 1 词表与领域模型 | `378acb4` | `domain/reasoning.rs`、`Request` 拆出 `CallOptions` |
| 2 能力与方言 | `2d92eca` | `provider/vendor/`（能力表 + 方言）、`ModelInfo.effort_support` |
| 3 归一化与映射 | `62d0084` | `provider/vendor/resolve.rs`、两个 codec 接线 |
| 4 展示与反馈 | 见下 | `ModelChoice`、picker 按能力生成、切换时的 warning |

---

## 附：genai 源码参考
| 位置 | 内容 |
| --- | --- |
| [`src/chat/chat_options.rs`](https://github.com/jeremychone/rust-genai/blob/v0.7.1/src/chat/chat_options.rs) | `ReasoningEffort`、`ChatOptions`、`ChatOptionsSet`（客户端默认 ← 单次覆盖） |
| [`src/chat/chat_stream.rs`](https://github.com/jeremychone/rust-genai/blob/v0.7.1/src/chat/chat_stream.rs) | `ChatStreamEvent`（Chunk / ReasoningChunk / ToolCallChunk / End） |
| [`src/adapter/adapter_types.rs`](https://github.com/jeremychone/rust-genai/blob/v0.7.1/src/adapter/adapter_types.rs) | `Adapter` trait，`to_web_request_data` 是各家写 wire 的地方 |
| [`src/adapter/adapter_kind.rs`](https://github.com/jeremychone/rust-genai/blob/v0.7.1/src/adapter/adapter_kind.rs) | 37 种 `AdapterKind` 与模型名路由规则 |
| [`src/adapter/adapters/anthropic/ant_reasoning.rs`](https://github.com/jeremychone/rust-genai/blob/v0.7.1/src/adapter/adapters/anthropic/ant_reasoning.rs) | effort → 各家词汇的映射与降级（静默） |
| [`src/adapter/adapters/anthropic/ant_model.rs`](https://github.com/jeremychone/rust-genai/blob/v0.7.1/src/adapter/adapters/anthropic/ant_model.rs) | 模型名 → 能力推导 |
| [`src/adapter/adapters/zai/adapter_impl.rs`](https://github.com/jeremychone/rust-genai/blob/v0.7.1/src/adapter/adapters/zai/adapter_impl.rs) | ZAI 复用 OpenAI 适配器（未实现 GLM 的 `thinking.type`） |
| `tests/data/yakbak/` | 录放式回归测试设施 |
