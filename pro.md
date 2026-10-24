# 思考强度（Reasoning Effort）实现现状与改造设计

针对 `crates/oven-llm` 及其下游（`oven-agent` / `oven-app` / `oven-tui`）中"思考强度"链路的完整调查结果，
以及"不同厂商/模型支持的档位不同"这一问题的设计方案。

结论先放这里：**能力必须表达四种状态（未知 / 不支持推理 / 只有开关 / 有档位），钳制只在请求边界做一次，
且请求路径永不报错；用户能操作的拒绝点在配置加载与 `/model` 切换。**

---

## 一、现状：实现逻辑

### 1.1 领域层：开关与档位刻意解耦

`crates/oven-llm/src/domain/request.rs` 定义了两个独立类型：

| 类型 | 位置 | 取值 | serde |
| --- | --- | --- | --- |
| `ThinkingMode` | `request.rs:114` | `Enabled` / `Disabled` | `"enabled"` / `"disabled"` |
| `ReasoningEffort` | `request.rs:133` | `None` / `Low` / `Medium` / `High` | `"none"` / `"low"` / `"medium"` / `"high"` |

`Request` 上是两个互不关联的字段（`request.rs:180-181`），类型均为 `Option<T>`：

- `Option::None` = 不下发该字段，交给上游默认
- `Some(ReasoningEffort::None)` = 显式关闭推理

两者都实现了 `Display`，直接输出 wire 风格小写串（`request.rs:121` / `request.rs:140`），
编码器不再各自重复 match。两套构造入口：`Request::builder().thinking(..).reasoning_effort(..)`
与可变式 `req.thinking(..)` / `req.reasoning_effort(..)`。

**关键约定**（`request.rs:389` 附近注释写死）：`thinking(Enabled)` **不隐含任何 effort**，
要档位必须再显式调 `reasoning_effort`。这是对早期行为的修正——CHANGELOG v0.3.0 曾让
`thinking(Enabled)` 默认填 `Medium`，后被移除，理由是"开关"和"档位"语义不同，隐式绑定会让用户
无法区分"我没设"和"系统替我设了 medium"。

### 1.2 协议映射：Completions 扁平，Responses 合并

| 领域字段 | Completions wire | Responses wire |
| --- | --- | --- |
| `thinking` | 独立字段 `thinking: {"type":"enabled"}`（`completions/types.rs:34`，类型见 `:53-56`） | 折叠进 `reasoning` 对象 |
| `reasoning_effort` | 独立字段 `reasoning_effort: "high"`（`completions/types.rs:36`） | `reasoning: {"effort":"high"}`（`responses/types.rs:48-50`） |

**Completions**（`completions/encoder.rs:81-85`）机械透传，两个字段各写各的，互不干涉——
同时设置时会同时出现 `thinking:{"type":"disabled"}` 和 `reasoning_effort:"none"`。

**Responses**（`responses/encoder.rs:340-351`）只有一个 `reasoning` 位，优先级：

1. 有 `reasoning_effort` → 直接用，**完全覆盖 thinking**（哪怕 thinking 是 `Disabled`）
2. 无 effort 但 `thinking == Disabled` → 折叠成 `{"effort":"none"}`
3. 无 effort 且 thinking 为 `Enabled` 或未设置 → **整个 `reasoning` 字段省略**

即 Responses 侧 thinking 与 effort 是同一条通道的两个表达，Completions 侧是两条独立通道。

### 1.3 思考内容的回传差异

- Completions：assistant 的 `Thinking` 块合并为 `reasoning_content` 回传
  （`completions/encoder.rs:268`、`:310`），因为交错式思考必须回传才能保证多轮上下文一致
- Responses：`Thinking` 块 v1 直接丢弃（`responses/encoder.rs:263`）

这与强度无关，但决定了"强度调高后多轮是否还保持同一推理轨迹"。

### 1.4 校验层对思考强度完全放行

`provider/validate.rs:117-163` 的 `validate_request` 只检查六项：max_tokens、system prompt、
tools、streaming、vision、context window。thinking 与 effort 一个都不查。
`ModelCapabilities` 里也**没有**任何"是否支持思考"的能力位。

`Thinking` 块的字符数照常计入 context 估算（`validate.rs:51`）。

### 1.5 Escape hatch：`provider_options`

`Request.provider_options` 是 `#[serde(skip)]` 的 provider 私有参数直通，由 transport 层 merge 进
wire body（`request.rs:182-185`）。需要非标准 effort 表达时（例如某些厂商用 `budget_tokens`
而非档位）可走这里，但键名必须与该厂商 wire format 一致，且不能与标准字段重名。

### 1.6 下游驱动方式

`oven-llm` 只提供类型与编码，策略在 `oven-agent`：

- `oven-agent/src/runtime/agent/request.rs:59-103` 的 `build_request()` 把 effort 当唯一真源反推 thinking：
  effort 存在且 ≠ `None` → `ThinkingMode::Enabled`，否则 → `ThinkingMode::Disabled`，两个字段一起下发。
  这是为了同时适配两套协议的差异。
- `oven-app/src/commands/model.rs:24` 暴露 `/model <model> <effort>`，effort 大小写不敏感解析
  （`none|low|medium|high`，`config.rs:429-437` 先 `to_ascii_lowercase`），只写 model 时保留当前 effort。
- `oven-app/src/runtime/shared/live.rs:176-214` 的 `resolve_model_switch()` 把 effort 落进
  `ProviderConfig` 并持久化。
- `oven-app/src/core/config.rs:86` 的 `ProviderConfig.reasoning_effort` 支持 TOML 配置。
- `/setup` 路径会在用户没填 effort 时补 `Medium`（`oven-app/src/runtime/mod.rs:426-427`）。
- `oven-proto/src/state.rs:43`、`:71` 把 `reasoning_effort` 挂在 `AgentState` 和 `ProviderView` 上。
- `oven-tui/src/widgets/status.rs:202-207` 在状态栏展示。
- `oven-agent/src/runtime/compact.rs:58-59` 压缩请求显式关思考：`thinking = Disabled`、`reasoning_effort = None`。

### 1.7 用量回读

`Usage.reasoning_tokens`（`domain/response.rs:17`）记录实际推理消耗：Responses 读
`output_tokens_details.reasoning_tokens`，Completions 读 `completion_tokens_details.reasoning_tokens`
并回落到 `prompt_tokens_details.reasoning_tokens`。这是验证某档位是否真的生效的唯一手段——
`oven-llm` 自己不校验档位是否被上游接受。

### 1.8 展示口径已经不一致

同一份状态在两个前端有两种说法：

- 状态栏 `model_label` 在 `None` 时**不显示**档位（`status.rs:202-207`）
- `/model` 查询把 `None` 打印成 `none`（`commands/model.rs:54-57` 的 `describe`）

也就是说"未指定"在 TUI 里被显示成"显式关闭"。这是 2.5 那个语义混淆的用户可见形态。

---

## 二、问题：厂商/模型的档位差异当前无法表达

### 2.1 三处证据

**能力模型里没有思考相关字段。** `ModelCapabilities`（`provider/model.rs:130-138`）共 7 个字段：
vision、tools、streaming、json_mode、parallel_tool_calls、system_prompt、max_concurrent_tools——
没有任何一个描述思考能力或档位集合。app 层的 per-model 元数据 `ModelMetadata`
（`oven-app/src/core/config.rs:143-150`）同样只有 6 个字段，也没有 effort 位，
所以连用户手写 TOML 声明模型档位都做不到。

**校验层不校验。** 见 1.4。

**UI 是硬编码常量。** `oven-tui/src/widgets/model_picker.rs:14-20` 的
`EFFORT_ITEMS: [(&str, &str); 5]` 固定为 none/low/medium/high/keep current，
**与用户在第一阶段选中的模型无关**。`ProviderConfig::parse_effort`（`config.rs:429-437`）
同样只认这四个词。

### 2.2 现在为什么还能跑通

靠的是**广播两个字段，让上游各取所需**。Completions 编码器同时发出：

```json
{ "thinking": {"type": "enabled"}, "reasoning_effort": "high" }
```

这两个字段分属两套厂商约定——`thinking.type` 是 Zhipu GLM 的开关写法（只有开/关，无档位），
`reasoning_effort` 是 OpenAI chat / Anthropic-compat 的档位写法。发出去后 GLM 认 `thinking`
忽略 `reasoning_effort`，OpenAI 系认 `reasoning_effort` 忽略 `thinking`。
`ReasoningEffort` 枚举本身（`domain/request.rs:133`）就是 OpenAI 风格的最小公共集。

**前提是上游宽容**——遇到不认识的 body 字段静默丢弃。一旦某个 provider 开始严格校验未知参数，
整个方案会从"能跑"变成"批量 400"。

### 2.3 失效场景

- **GLM 类模型上选 high**：GLM 没有档位概念，`reasoning_effort: "high"` 被静默忽略。
  用户看到状态栏显示 `glm-5.3 high`，以为强度生效了，实际就是普通推理模式。**静默失效比报错更糟。**
- **老 OpenAI chat 模型上选 none**：`reasoning_effort: "none"` 不在 low/medium/high 集合里。
  支持 none 的模型集合随厂商和版本变化，不在本文猜测之列；不支持时行为是 400 或静默忽略。
- **不支持推理的模型上选 high**：同样静默忽略，或严格校验时 400。

错误发生在**请求时**（上游返回 400），不是**切换时**——`/model gpt-4o high` 那一步没有任何反馈，
用户要到发消息才发现失败。

### 2.4 现有 bug：unset ≠ 不下发

`oven-agent/src/runtime/agent/request.rs:62-66` 的推导逻辑是
「effort 为 `None` 或等于 `ReasoningEffort::None` → `thinking = Disabled`」。
而 Responses 编码器（`responses/encoder.rs:344-350`）在「无 effort 但 thinking 为 Disabled」时
会折叠出 `{"effort":"none"}`。所以**只要某条配置路径留下了 unset，Responses 协议下就会显式下发
`effort: none`**，把上游默认档位（比如 o3 的 medium）强行压到关闭。

哪些路径会留下 unset：手写 `config.toml`、`OVEN_MODEL` / `OVEN_BASE_URL` 环境变量、
未走 `/setup` 的 provider。（走 `/setup` 的会被补成 `Medium`，`runtime/mod.rs:426`；
所以这不是"所有人都会中"，但它**恰好是配置文件手写党的默认路径**。）

`None` 在领域层表示"不下发"，到 wire 层却变成了"显式关闭"，这两层语义是拧的。

### 2.5 语义是四态，不是两态

真正需要表达的是四种情况，今天全被压进两个 `Option`：

| 意图 | 今天怎么表达 | 落到 wire 的后果 |
| --- | --- | --- |
| 未指定 | `None` + agent 补 `Disabled` | Responses 变成 `effort: none`（2.4） |
| 显式关闭 | `Some(ReasoningEffort::None)` + `Disabled` | 同上，两者无法区分 |
| 某档位 | `Some(Low/..)` + `Enabled` | 广播，靠上游挑 |
| 模型没有档位（只开关） | 无表达 | 无从降级 |

外加模型侧还有第四种：**不支持推理**（两个字段都该不下发）。所以类型设计里
"未指定 / 关闭 / 有档位"三分不够，必须给"只有开关"和"完全不支持"各留一个位置，见 3.2。

### 2.6 compact 与能力模型直接冲突

`compact.rs:58-59` 无条件写 `thinking = Disabled` + `effort = None`。在"只能是 low/medium/high"
的 Responses 模型上，这会被编码成 `effort: none`（2.4 同一条链路）而 400，压缩功能当场坏掉。
如果按直觉把"模型不支持关闭就报错"放在请求路径（前一版 3.4 正是这么写的），compact 会先挂。
所以"关闭"必须允许向下兜底，见 3.4。

### 2.7 目录档位不可维护

已发布模型的档位只能靠人手抄进 `catalog.rs`，而厂商改档位、发新模型时没人会回来改。
没有校准手段的话，这份数据半年后就是谣言。缓解方案见 3.8。

---

## 三、设计方案

### 3.1 词表层：扩到阶梯

实际用例（deepseek 系 `none/low/high/max`、minimax 系 `low/medium/high/xhigh/max`）的并集
至少 6 档；OpenAI 的 `gpt-5` 时代还出现过比 `low` 更弱的 `minimal`（落地前请用厂商文档确认，
见 7.2）。

```rust
pub enum ReasoningEffort {
    #[serde(rename = "none", alias = "off")]
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}
```

`Off` 取代 `None`（wire 值仍是 `"none"`，另加 `alias = "off"` 接受用户手写），语义自明，
消除 2.5 的双重 `None`。注意 `#[serde(rename_all = "lowercase")]` 对 `Off` 会产出 `"off"`，
必须显式 `rename` 才能保住兼容；`XHigh` → `"xhigh"` 无需额外处理。

**支持集是非连续的**（deepseek 没有 medium，minimax 没有 none），所以钳制**不能按索引取**，
必须按强度序号比较：

```rust
impl ReasoningEffort {
    /// 强度序号。`Off` 是"关闭"而非某个档位，不在阶梯上。
    pub const fn rank(self) -> Option<u8> {
        match self {
            Self::Off => None,
            Self::Minimal => Some(0),
            Self::Low => Some(1),
            Self::Medium => Some(2),
            Self::High => Some(3),
            Self::XHigh => Some(4),
            Self::Max => Some(5),
        }
    }
}
```

`Off` 返回 `Option::None` 而不是 0 是刻意的——强制调用方单独处理"关闭"这条语义路径，
不让它被当成"最弱档"混进阶梯。

> 若某厂商档位拼写与上面不同（例如 `x-high`），在编码器里做映射，不要为它改枚举。

### 3.2 能力层：四态枚举，而不是 `Option<Vec>`

前一版用 `Option<Vec<ReasoningEffort>>`（`None`=未知、`Some(vec![])`=不支持）表达，但
**"只支持开关、不支持档位"（GLM 类）在两者之间没有位置**，而它和"完全不支持"在编码器里走
完全不同的分支（一个是发 `thinking.type`，一个是两个字段都不发）。用空 Vec 编码第三种，
等于又造一个隐式约定。

```rust
pub enum EffortSupport {
    /// 未声明：全部放行，保持"没声明即支持"的既有哲学。
    Unknown,
    /// 完全不支持推理：两个字段都不下发。
    Unsupported,
    /// 只有开/关，没有档位（GLM 类）：档位请求折叠成 `thinking.type`。
    Toggle,
    /// 有档位，构造时排序去重，**不允许为空**（空集请写 Unsupported）。
    Levels(Vec<ReasoningEffort>),
}
```

放在 `provider/model.rs`，与 `ModelInfo` 同层；`ModelInfo` 加字段：

```rust
pub struct ModelInfo {
    // ...
    pub effort_support: EffortSupport,
}
```

默认 `Unknown`。`ModelInfo` 有 5 处结构体字面量构造（`catalog.rs:57`、`router.rs:329`、
`validate.rs:178`、`list_models.rs:92`、`runtime_test.rs:2228`），另有 `declared` / `minimal`
两个构造函数；加字段要一并补上，都取 `Unknown` 默认。

放 `ModelInfo` 而不是 `ModelCapabilities` 的理由不变：后者是 `Copy` 且语义是布尔能力位，
塞进 `Vec` 会逼着 `with_overrides` 改签名；`validate_request(req, model, stream)` 本来就拿到
`&ModelInfo`。

### 3.3 配置层：接缝已经在了，但作用域要说清

`ModelMetadata`（`oven-app/src/core/config.rs:143-150`）是 per-model 元数据的唯一入口：

| 位置 | 职责 |
| --- | --- |
| `config.rs:173-186` `apply_to` | 把声明的字段打到 `ModelInfo` 上（只有 `Some` 才覆盖） |
| `config.rs:160-169` `from_info` | 反向，目录条目回填成元数据（保存配置时不丢字段） |
| `config.rs:190-209` `merge` + `config.rs:49-55` `pick` | 用户配置 / 项目 `.oven.toml` / 运行时 overlay 三层合并 |
| `config.rs:115` `ProviderRest.reasoning_effort` | provider 表里的扁平键 |

改动是加一个字段 + `apply_to` 里一行 + `from_info` 一行 + `merge` 一行：

```rust
pub struct ModelMetadata {
    // ...现有 6 个字段
    pub effort_support: Option<EffortSupport>,
}
```

TOML 形态（用 `#[serde(untagged)]` 让字符串与数组两种写法共存）：

```toml
[providers.zhipu.models."glm-5.3"]
effort_support = "toggle"

[providers.xai.models."grok-4.6"]
effort_support = "unsupported"       # 完全不支持推理

[providers.deepseek.models."deepseek-v4-flash"]
effort_support = ["none", "minimal", "low", "high", "max"]

[providers.myproxy.models."my-model"]
effort_support = ["low", "medium", "high"]
```

不写这个键 = `Unknown`（全放行）。`[]` 直接**反序列化报错**，提示改写成 `"unsupported"`——
空数组和"不支持"、"只有开关"都会混，不给它留第三种读法。

> **更正（前一版是错的）**：provider 表里的扁平键**只对 `models` 里显式声明过的模型生效**，
> 对内置 catalog 条目无效。`effective_models()` 只遍历 `models` 表，provider 表头只是给这些
> 条目 `fill_missing` 的默认值（`config.rs:475-482`），而 catalog 模型来自
> `ProviderBuilder` 的 preset，`build_client_with` 只把 `effective_models()` 里的条目
> `add_model` 进去（`core/provider.rs:145-151`）。所以下面这种写法**不会**作用于内置模型：
>
> ```toml
> [providers.deepseek]
> effort_support = "toggle"        # 对 catalog 里的 deepseek 模型无效
> ```
>
> 要么在文档里写清"provider 级默认只对已声明模型生效"，要么另做一次决策：让 provider 级
> metadata 也覆盖 catalog 条目（要改 `build_client_with` 的取模型逻辑）。**不要按错误假设开工。**

### 3.4 钳制语义：只向下，请求路径不报错

把"意图 → 最终下发"做成一个纯函数，输入是请求意图和能力，输出是规范化的
`(effort, thinking)` 对：

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reasoning {
    pub effort: Option<ReasoningEffort>,
    pub thinking: Option<ThinkingMode>,
}

pub fn resolve_reasoning(
    effort: Option<ReasoningEffort>,
    thinking: Option<ThinkingMode>,
    support: &EffortSupport,
) -> Reasoning
```

表里 `Unknown` 一行是"完全不改"，其余三行由能力决定；`thinking` 单独有值（compact）的情况见第 4 条。

| support | 未指定（两个都空） | 显式 `Off` | 档位 L |
| --- | --- | --- | --- |
| `Unknown` | 原样（两个字段都不改） | 原样 | 原样 |
| `Unsupported` | 空 | 空（记 warn） | 空（记 warn） |
| `Toggle` | 原样 | `effort` 清空，`thinking = Disabled` | `effort` 清空，`thinking = Enabled` |
| `Levels(S)` | 空 | `thinking` 清空；含 `Off` → `effort = Off`，否则取 `S[0]` | `thinking` 清空；`S` 中 `rank ≤ rank(L)` 的最高档，都比 L 强则取 `S[0]` |

四条规则值得单说：

1. **"未指定"在任何 support 下都产出空**，不下发任何字段。这是 2.4 的根治点，和 2.5 的四态对齐。
2. **向下钳制永不报错**。模型不支持 `Off` 时降到最低可用档，不然 compact（2.6）和后台请求会挂。
   用户的"我想关掉推理"的拒绝放在**切换时和配置加载时**（3.6），那里才有 actionable 的反馈。
3. **`Unknown` 不收窄通道**，保留"两个字段都发"的广播。那正是 2.2 里靠上游挑的兜底策略，
   在我们确实不知道模型能力时是唯一可行的；只有能力已知（`Toggle` / `Levels` / `Unsupported`）
   才收窄成单一通道。因此能力模型对被 catalog 覆盖的模型是**纯增益**，对没覆盖的模型不改变行为。
4. **`thinking` 只在支持集给不出档位时才由能力决定**。`Toggle` 且 `effort` 未给时保留传入的
   `thinking`（compact 今天正是这么显式关掉思考的），不被函数顺手抹掉。


`Levels` 的不变量（升序、去重、非空）在构造/反序列化时保证，这样 `resolve_reasoning` 里
取 `S[0]` 和"最后一个满足 rank ≤ L"是 O(n) 且无歧义。

### 3.5 规则执行位置：`Endpoint` 的请求边界，encoder 不动

编码器拿不到 `ModelInfo`，但两个协议 provider 都已经在 `complete` / `stream` 里走同一个入口：

```rust
// crates/oven-llm/src/provider/endpoint.rs:58
pub(crate) fn validate_known_model(&self, req: &Request, stream: bool) -> Result<()>
```

把它升级成 `prepare`，一次做完"解析模型 → 解析推理意图 → 校验"，返回 `Cow`：

```rust
pub(crate) fn prepare<'a>(
    &self,
    req: &'a Request,
    stream: bool,
) -> Result<Cow<'a, Request>, ProviderError> {
    let Some(model) = self.resolve_model(&req.model) else {
        return Ok(Cow::Borrowed(req));   // 未知模型：宽松透传，与今天一致
    };
    let reasoning = resolve_reasoning(req.reasoning_effort, req.thinking, &model.effort_support);
    let mut prepared = Cow::Borrowed(req);
    if reasoning.effort != req.reasoning_effort || reasoning.thinking != req.thinking {
        let mut owned = req.clone();
        owned.reasoning_effort = reasoning.effort;
        owned.thinking = reasoning.thinking;
        prepared = Cow::Owned(owned);
    }
    validate_request(&prepared, model, stream)?;
    Ok(prepared)
}
```

两个 provider 各两处（`completions/provider.rs:191`、`:211`；`responses/provider.rs:172`、`:192`），
各加一行 `let req = self.endpoint.prepare(req, stream)?;`。收益：

- **encoder 零改动**：它继续只读 `thinking` / `reasoning_effort` 两个字段；能力已知的模型由
  `prepare` 收窄成单一通道，未知模型保持原来的广播，不用改签名、不用改那一堆编码器测试；
- **一个规则、一个落点**，不依赖 caller 记得钳制，`compact`、子 agent、以后可能的 embed 都覆盖；
- **热路径不额外拷贝**：`Cow` 只在真的发生调整时才 clone，正常请求零成本；
- 被调整时在 `prepare` 里落一条结构化日志（model / requested / effective / support）。

`validate_request` 收到的已经是钳制后的请求，新增的 effort 校验因此只做不变量兜底
（值必须落在支持集内），正常路径不可能触发；它真正防的是绕过 `prepare` 直接调用校验的代码。

### 3.6 早期反馈：加载、启动、切换、spawn

请求路径既然不报错，用户能看见的拒绝点就得补在动作发生时：

| 时机 | 位置 | 行为 |
| --- | --- | --- |
| TOML 解析 | `config.rs` derive | 档位/`effort_support` 拼错 → 加载即报 `unknown variant` |
| 启动 | 启动时把 `ProviderConfig.reasoning_effort` 装进 Selection 的地方（`runtime/mod.rs:464-466` 附近） | 与该模型能力比对，不可达就降级 + 一次性 warn，**不阻止启动** |
| `/model` 切换 | `commands/model.rs` + `runtime/shared/live.rs:176` | 显式 `Off` 但模型无 `Off`、或档位落在 `Unsupported` 上 → 返回错误并列出可用档位，**不落盘** |
| 子 agent spawn | `oven-agent/src/core/subagent.rs`（携带 `reasoning_effort`） | 用同一个 `resolve_reasoning`，被调整时 warn |

`resolve_model_switch` 已经拿到 `&Router`，`format_model_switched`（`live.rs:115`）也已经在
拼提示文本，把"请求 → 有效"的差异放进去是一行的事。

### 3.7 展示层：显示有效值，不显示请求值

2.1 抱怨的"显示 high 实际不是 high"，如果钳制后仍显示请求值，只是换了种形式复发。
所以状态栏和 `/model` 查询都渲染**有效值**（`resolve_reasoning` 的输出）：

- `effort = Some(Low)` → `low`
- `thinking = Some(Enabled)` 且无档位 → `on`（Toggle 模型）
- `thinking = Some(Disabled)` → `off`
- 两者皆空 → 不显示 / `未指定`

`describe`（`commands/model.rs:54`）今天把 `None` 打印成 `none`，要一并改掉；这是同一批里
最容易漏、又最影响信任度的一行。

proto 侧 `AppLevels.reasoning_effort: Option<ReasoningEffort>` 换成
`reasoning: Reasoning`（两个字段都 `Copy`，无分配）。`ProviderView.reasoning_effort`
语义不同（它存的是**配置里的请求值**），保留不动，必要时改名为 `configured_effort`。

### 3.8 目录数据来源：按厂商默认，再校准

档位数据别拍脑袋写。分两步：

1. catalog 只按厂商给默认值（Zhipu → `Toggle`；OpenAI/DeepSeek → 已知档位集；
   其余 → `Unknown`），个别模型再覆盖。`Unknown` 承担"没维护"的成本，不牺牲正确性上限。
2. 用 `Usage.reasoning_tokens` 跑一个 model × effort 矩阵来校准（这也是 1.7 说的唯一验证手段）。
   可以作为 `/dev-probe` 之类的显式命令，只在开发时用，结论写回 catalog。

不这么做的代价是：每加一个模型都要人肉查文档，写错了没人知道。

### 3.9 关于 `Option<String>` 方案

若把 `ReasoningEffort` 改成字符串承载（newtype over `String` + `LADDER` 常量表）：

| | 封闭枚举 | String |
| --- | --- | --- |
| 新增厂商档位 | 改代码 + 发版 | 只改表 |
| 配置拼写错误 | 加载即报错 | 静默通过，到上游才 400 |
| 顺序 | 类型保证 | 运行时查表，查不到就没有序 |
| Copy | 有 | 无 |
| 穷尽匹配 | 有 | 无 |

**结论：不推荐**。真正的问题是"钳制需要一个全局强度序"，而这个序无论用类型还是用表表达，
都是一份封闭名单——String 化只是把封闭性从编译期挪到运行时，没有消除它。具体代价：

- `ProviderRest.reasoning_effort`（`config.rs:115`）走 derive 的 `Deserialize`，
  现在写 `reasoning_effort = "hih"` 会在加载时报 `unknown variant 'hih', expected one of ...`；
  换 String 就变成静默接受。
- 表外档位没有序，也就无法参与钳制，得单独定策略。
- 丢失 `Copy`——`Option<ReasoningEffort>` 在 `AgentState`（`oven-proto/src/state.rs:43`）
  和 `ProviderConfig` 里到处要 clone。

补充理由：不同厂商的档位名意外对齐（low/medium/high/xhigh/max 语义一致），
这让一个全局阶梯真的成立——这恰恰是 String 化拿不到的。
真要留逃生口，参照仓库先例 `ToolChoice::Tool(String)` 加 `Custom(String)` 变体，
代价是丢 `Copy` + 手写 Serialize/Deserialize。

### 3.10 版本与兼容

`None → Off` 改名、加新变体、给 `ModelInfo` 加字段都是 **breaking**（`oven-llm` 是公开 crate，
`oven-proto` 也带着这个枚举）。处理方式：

- 走 minor 升版 + `!` 提交（仓库已有 `refactor(app)!:` 的先例），并在 CHANGELOG 写明；
- serde 侧靠 `#[serde(rename = "none")]` 保持 TOML 与 proto 的 wire 兼容，
  老配置文件不会因为改名读不出来；
- 用户手写 `off` 通过 `alias` 接受，不用做迁移。

---

## 四、需要改动的文件

| 文件 | 改动 |
| --- | --- |
| `oven-llm/src/domain/request.rs` | 枚举扩档，`None` → `Off`，加 `rank()`；新增 `Reasoning` |
| `oven-llm/src/provider/model.rs` | 新增 `EffortSupport`，`ModelInfo.effort_support`，构造函数补默认 |
| `oven-llm/src/provider/endpoint.rs` | `validate_known_model` → `prepare`（`Cow`，解析 + 钳制 + 校验 + 日志） |
| `oven-llm/src/provider/{completions,responses}/provider.rs` | 各两行，改用 `prepare` |
| `oven-llm/src/provider/validate.rs` | 兜底校验 effort/thinking 与 `effort_support` 一致 |
| `oven-llm/src/provider/catalog.rs` | 按厂商默认填 `effort_support` |
| `oven-app/src/core/config.rs` | `ModelMetadata.effort_support` + `apply_to` / `from_info` / `merge` |
| `oven-app/src/core/provider.rs` | `declared_model_info` 带上能力（`apply_to` 覆盖即可） |
| `oven-app/src/commands/model.rs` | `parse_effort` 认新档位；切换时钳制 + 报错；`describe` 显示有效值 |
| `oven-app/src/runtime/shared/live.rs` | `resolve_model_switch` 落有效值 + 提示文案 |
| `oven-app/src/runtime/mod.rs` | 启动时校准；`refresh_model_choices`/`merge_model_choices` 带出 `effort_support` |
| `oven-agent/src/runtime/agent/request.rs` | thinking 由 `if/else` 改为 `effort.map(..)`：unset 不下发，档位仍广播（0 批） |
| `oven-agent/src/runtime/compact.rs` | 2 批改用 `Some(Off)` 表达意图，交给 `prepare` 按能力降级 |
| `oven-agent/src/core/subagent.rs` | spawn 时按模型能力校准并 warn |
| `oven-proto/src/state.rs` | `AppLevels.reasoning: Reasoning`；`models` 换具名结构（见五·6） |
| `oven-tui/src/widgets/model_picker.rs` | `EFFORT_ITEMS` → 按所选模型的 `effort_support` 生成 |
| `oven-tui/src/widgets/status.rs` | 渲染有效值（`on` / `off` / 档位 / 不显示） |
| `oven-app/config.example.toml` | 声明语法 + provider 级作用域说明 |

---

## 五、六个坑

1. **四态，不是三态**。`Unknown` / `Unsupported` / `Toggle` / `Levels` 各有一条编码路径。
   `Option<Vec>` 表达不了 `Toggle`，空 Vec 的语义又会和 `Unsupported` 打架。

2. **"未指定"必须一路保持不下发**。这是 0 批修复的核心，也是唯一会改变现有默认行为的改动：
   今天未配置 effort 的 provider 默认 thinking 关闭，修完变成"交给上游"（GLM 一类可能默认开思考，
   成本上升）。**注意只收窄 unset 这一种情况**——显式档位仍要保留双字段广播（3.4 第 3 条），
   否则没被 catalog 覆盖的 GLM 兼容代理会失去 `thinking.type=enabled`。取舍要先拍板（7.1）。

3. **钳制做一层，别做两层**。规则只有一个 `resolve_reasoning`；落点只有 `prepare` 一个；
   命令层和启动层只负责"提前反馈"，不各自实现降级。否则规则必然漂移。

4. **别为了钳制 clone 整个 `Request`**。`Request` 带着完整历史，hot path 上每次 clone 都是浪费。
   用 `Cow`，只在真的发生调整时 owned。

5. **静态目录是主来源，config 是覆盖，但 provider 级覆盖只对已声明模型生效**（3.3 的更正）。
   `apply_to` 是字段级覆盖（只有 `Some` 才打），`effort_support` 整组替换，不逐元素合并。

6. **`AppLevels.models: Vec<(String, String)>` 这条元组要趁早换成具名结构**。
   picker 要按模型生成档位，就必须把 `effort_support` 从 `merge_model_choices`
   （`runtime/mod.rs:690-720`，今天只留 id + provider 字符串）带出来。
   改成 `ModelChoice { id, provider, effort_support }` 一次到位，别再往元组里塞第三项。
   注意 `/models` 端点动态拉到的模型没有能力信息，只能是 `Unknown`（全放行）。

---

## 六、建议的落地顺序

**第 0 批（独立可发，不依赖任何新类型）**

决定 7.1 的默认语义；`build_request` 里把 thinking 的 `if/else` 换成
`reasoning_effort.map(|e| if e == Off { Disabled } else { Enabled })`——未指定不再下发任何推理字段，
显式档位仍保持双字段广播（未知模型的兜底）；`describe` 不再把 unset 打印成 `none`；
`compact` 本批不动（它的意图改写在 2 批跟 `prepare` 一起做）。
验收：一条 wire 断言——未配置 effort 时两个协议都不带任何推理字段。这条测试就是 2.4 的回归网。

**第 1 批（可独立验证的切片）**

词表扩展 + `Off` 重命名 + `rank()` + `EffortSupport` + `ModelInfo.effort_support` +
catalog 厂商默认 + `ModelMetadata` 通路 + `validate_request` 兜底。
验收：档位能被声明；拼错在加载时报；未声明仍全放行；`[]` 被拒绝。

**第 2 批（行为正确性）**

`Reasoning` + `resolve_reasoning` + `prepare` + 两个 provider 接线 + 切换/启动/spawn 的
早期反馈 + 结构化日志。
验收：`effort_support × 协议 × 意图` 的矩阵测试（4 × 2 × 4 = 32 条 wire 断言，表驱动）；
能力已知的模型上广播字段消失、`Unknown` 保持原样；`compact` 在无 `Off` 的模型上不再 400。

**第 3 批（可见性）**

`AppLevels.reasoning` + `ModelChoice` + picker 动态化 + 状态栏/`/model` 显示有效值 +
`config.example.toml`。

**档位层（在第 1 批之后，见第八章）**

A 档：`TierTable` + TOML 解析 + `/tier` 命令 + 有效值展示。
B 档：provider 切换/启动时按 `active_tier` 重新展开，手工 `/model` 解绑档位。
C 档：picker 档位层 + 状态栏显示 `档位 → 模型 · 有效 effort`。

---

## 七、待决策

1. **未配置 effort 的产品语义**：改成"不下发、交给上游"
2. **`minimal` 是否纳入阶梯**。 不考虑，并删除
3. **目录精度**：先厂商默认 + `Unknown`（省，够用）。
4. **"开但不定档"要不要单独表达**（Toggle 模型上的 `on`）。不设置档位，认为就是 `on`
5. **切换时遇到不可达的档位**：直接拒绝，要求重选
6. **档位只在 active provider 内解析**（不跨厂商路由）：`medium` 永远展开为当前 provider
   的模型 + effort，切 provider 后重新展开
7. **档位是预设还是持续约束**：选预设——手工 `/model` 即解绑档位（八·8.5）
8. **档位名是否复用 effort 词表**：不复用，用自由词（`quick/balanced/deep`）避让撞名（八·8.7·1）

---

## 八、档位层（tier）：把「档位」这个词从 effort 里拆出来

### 8.1 定位：档位在 effort 钳制之上

本文解决的是「**一个模型**支持哪些 effort、请求时怎么钳」。
档位解决的是另一件事：「**一个抽象质量级别**在每个厂商各自落到哪个模型 + 哪个 effort」。

用户口中的「我们的 medium 对应 deepseek-flash 的 medium、kimi-k3 的 high」属于后者：
`medium` 是一个**跨厂商的质量标签**，每个厂商用自己的模型和 effort 词汇去实现它。

> 术语：本文一到七章的「档位」指的是厂商的 effort 档位（`low/medium/high`）；
> 第八章起，「档位」特指 tier 这个产品概念，用 `tier` 标注以免混淆。

所以档位是本文之上的一层，链路是：

```
tier "medium" --(按厂商查表)--> (model, effort)
             --(本文的 resolve_reasoning 向下钳制)--> wire
```

档位表里的 effort 仍然要过 3.4 的钳制：档位写 `high`、模型只支持 `low/medium`，
最终落到 `medium`，并按 3.6 的规矩在切换时告知，而不是请求时 400。
档位层**不重复实现钳制**，只提供「意图 → (model, effort)」的查表。

### 8.2 数据模型

档位名是**全局的**（“我们的 medium”），落地方式是**每厂商一份**。表按档位分层、厂商作为第二层：

```toml
[tiers.medium]
deepseek = { model = "deepseek-v4-flash", effort = "medium" }
moonshot = { model = "kimi-k3",        effort = "high"   }

[tiers.high]
deepseek = { model = "deepseek-v4-pro", effort = "high" }
moonshot = { model = "kimi-k3",        effort = "xhigh" }
```

放在 oven-app 的配置层（不是 oven-llm）：它是产品概念，而且要覆盖自定义厂商。

```rust
pub struct TierBinding {
    /// wire id，属于该 provider
    pub model: String,
    pub effort: Option<ReasoningEffort>,
}
/// 档位名 -> (厂商 slug -> 绑定)
pub type TierTable = BTreeMap<String, BTreeMap<String, TierBinding>>;
```

`effort` 是 `Option`：`None` = 该档位不下发 effort 字段（配合 3.4 的「未指定」语义）。

### 8.3 解析规则与落点

- **解析只在动作发生时**：档位切换、provider 切换、启动时。请求路径不查表——
  agent 侧拿到的永远是已解析好的 `(ModelId, Option<ReasoningEffort>)`，与今天完全一致。
- **查表**：`(tier, active_provider) -> TierBinding`。该厂商没有这条绑定 →
  **拒绝切换并列出该厂商可用的档位**，不落盘（沿用七·5 的“切换时拒绝”）。
- **钳制**：查到的 `effort` 再交给 3.4 的 `resolve_reasoning` 按该模型的 `EffortSupport` 向下钳制。
- **展示**：显示**有效值**，不是档位里的原值。档位与有效值不一致时两段都显示
  （`medium → kimi-k3 medium`），否则就是 3.7 那个「显示 high 实际不是 high」的复发。

落点：

| 时机 | 位置 | 行为 |
| --- | --- | --- |
| 档位切换 | `oven-app/src/commands/`（新命令 `/tier`） | 查表 + 钳制 + 落盘；不可达则拒绝 |
| provider 切换 | `runtime/mod.rs:407` `set_provider` | 按 `active_tier` 重新展开；展开结果驱动 model / effort / context window |
| 启动 | `runtime/mod.rs:464` 附近 | 按 `active_tier` 展开；不可达则降级 + 一次性 warn，不阻止启动 |
| 手工 `/model` | `commands/model.rs` | **解绑档位**（见 8.5） |

### 8.4 状态与优先级

- `active_tier: Option<String>` 存 `AppConfig` 根部（`core/config.rs:566` `AppConfig`），
  它是**全局意图**，不属于任何单个 provider。
- `ProviderConfig.model` / `ProviderConfig.reasoning_effort` 存该厂商下**已解析的结果**，语义不变。
- 优先级：**档位展开结果 > provider 表手写值**；无档位时手写值生效。
  两者都写时档位赢，但必须有一处显式判优先级，否则「档位展开的 model」和
  「用户手写的 model」会互相覆盖。

### 8.5 档位是预设，不是持续约束

用户手工 `/model` 一次就脱离了档位。所以：**手工切换即解绑**（`active_tier = None`）。

另一种设计是让档位成为持续约束（每次换 provider 都重新展开、每次手工切换都被覆盖回去）。
两者语义完全不同，不能含糊——前者是「快捷方式」，后者是「策略」。
本文选前者，符合五·3「规则只有一处、命令层只做反馈」。

### 8.6 与本文各批次的依赖

档位要写 effort 值，就必须先有第 1 批的词表（`Off/Minimal/Low/Medium/High/XHigh/Max`）
和 `EffortSupport`，否则：

- 档位表里写 `xhigh` 在加载时就报 unknown variant；
- 钳制无处可去，档位给出的 effort 可能直接 400。

所以顺序是 **第 1 批 → 档位**。第 0 批（unset 不下发）与档位正交，可先发。

### 8.7 六个坑

1. **命名撞车**：档位叫 `medium`，effort 也叫 `medium`。必须分域——`/tier medium` 命令
   vs `/model <id> <effort>` 的第二参；TOML 里 `[tiers.medium]` vs `reasoning_effort = "medium"`。
   建议档位名用**自由词**（`quick/balanced/deep`），不复用 effort 词表，
   否则 UI 上永远要说“medium 档位的 medium effort”。
2. **双重真相**：档位展开出的 model 与用户手写的 model 谁赢，见 8.4 的优先级；再加 8.5 的解绑，
   这两处必须一起实现，否则会出现「档位是 medium 但跑的是手写模型」的错位状态。
3. **跨厂商等价关系会烂**：kimi-k3 的 high ≈ deepseek-flash 的 medium 是主观判断，
   随厂商版本漂移。必须允许用户覆盖；内置默认档位表只当**启发式**，不当事实
   （同 3.8 对目录数据的态度）。
4. **切档位会换模型** → 上下文窗口、价格、工具能力、compact 阈值全变。
   `set_provider` 之后那段 `context_window_of` / `refresh_model_choices`
   （`runtime/mod.rs:482`）都要跟着走一遍。
5. **子 agent**：subagent 沿用父 agent 的 model + effort
   （`oven-agent/src/core/subagent.rs:66`）。档位要不要让子 agent 用不同档位
   （如 explore 用 quick）是独立问题，本文不涉及。
6. **拒绝点位置**：档位查不到绑定时拒绝（切换时），查到了但 effort 不可达时钳制并告知（切换时）。
   请求路径永远不报错——和 3.4 的规矩一致。

### 8.8 落地三步（在本文各批之后）

- **A 档（数据）**：`TierTable` + TOML 解析 + `/tier` 命令（查询/切换）+ 有效值展示。
  依赖第 1 批的 effort 词表。
- **B 档（联动）**：provider 切换与启动时按 `active_tier` 重新展开；不可达则降级 + warn；
  手工 `/model` 解绑档位。
- **C 档（UI）**：picker 增加档位层或独立面板；状态栏显示 `档位 → 模型 · 有效 effort`；
  `config.example.toml` 补声明语法。

---

## 九、修订记录

相对上一版修正了以下问题：

- **provider 级默认值的作用域写错了**：它只作用于 `models` 里声明过的模型，不作用于内置 catalog（3.3）。
- **`Option<Vec<Effort>>` 三态不够**：改为 `EffortSupport` 四态，补上"只有开关"（3.2）。
- **钳制落点从"给 encode 传 `Option<&[Effort]>`"改为 `Endpoint::prepare`**：encoder 零改动，
  热路径用 `Cow`，覆盖所有调用方（3.5）。
- **补 `compact` 冲突**：原设计"不支持 Off 就报错"会让压缩请求 400；改为请求路径只向下兜底，
  拒绝点上移到切换/加载（2.6、3.4、3.6）。
- **补展示一致性**：钳制后必须显示有效值，`describe` 把 unset 显示成 `none` 要一起改（1.8、3.7）。
- **词表补 `minimal`**、`off` 别名与 semver 说明（3.1、3.10）。
- **落地顺序前置"0 批"**：把与能力模型无关的 unset 修复单独发，先解除 2.4 的影响（六）。
- **`Unknown` 保留双字段广播**：0 批只收窄 unset，否则没被 catalog 覆盖的 GLM 兼容代理会丢掉
  `thinking.type=enabled`；能力模型对已知模型收窄，对未知模型零行为变更（3.4、五·2、六）。
- **`AppLevels.models` 元组**换成具名结构，避免 picker 需求落地时再改一次 proto（五·6）。

### 相对上一版新增

- **新增第八章「档位层」**：把「档位」这个词从 effort 里拆出来——档位是跨厂商的质量标签，
  落在本文的 effort 钳制之上（查表 → (model, effort) → 钳制 → wire）。
  数据模型 `[tiers.<档位>.<provider>]`、只在 active provider 内解析、档位是预设（手工切换即解绑）、
  拒绝点仍在切换时、请求路径不查表。依赖第 1 批的 effort 词表，因此排在各批之后。
- **待决策补 6–8 条**：档位的解析范围、预设 vs 约束、命名域。
