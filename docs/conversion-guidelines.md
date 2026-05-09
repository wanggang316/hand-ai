# TypeScript → Rust 转换指导原则

> 将 pi-mono (TypeScript monorepo) 转换为 hand-ai (Rust workspace) 的指导原则。
> 目标：写出地道的 Rust 代码，而非机械翻译。

---

## 1. 项目结构映射

| TypeScript 概念 | Rust 对应 | 说明 |
|----------------|-----------|------|
| npm workspace | Cargo workspace | 已有 `Cargo.toml` workspace 配置 |
| package | crate | `crates/` 目录下每个子目录对应一个 crate |
| `index.ts` (导出) | `lib.rs` (pub mod + pub use) | Rust 通过 `pub` 可见性控制公开 API |
| `package.json` | `Cargo.toml` | 依赖、元信息、features 等 |
| `tsconfig.json` path aliases | Cargo workspace dependencies | workspace 级别统一依赖版本 |

### Crate 命名

| TS Package | Rust Crate | 说明 |
|-----------|------------|------|
| `@mariozechner/pi-ai` | `hand-model` | **已完成大部分**，LLM API 统一层 |
| `@mariozechner/pi-agent-core` | `hand-agent` | Agent 运行时 |
| `@mariozechner/pi-coding-agent` | `hand-coding-agent` | 编码 Agent CLI |
| `@mariozechner/pi-tui` | `hand-tui` | 终端 UI |
| `@mariozechner/pi-web-ui` | — | 暂不转换（Web UI 保持前端技术栈更合理） |
| `@mariozechner/pi-mom` | — | 暂不转换（Slack bot，优先级低） |
| `@mariozechner/pi` (pods) | — | 暂不转换（GPU 管理，独立关注点） |

---

## 2. 类型系统转换

### 2.1 基本类型映射

| TypeScript | Rust | 备注 |
|-----------|------|------|
| `string` | `String` / `&str` | 所有权场景用 `String`，借用用 `&str` |
| `number` | `i32` / `u32` / `i64` / `u64` / `f64` | 按实际语义选择精确类型 |
| `boolean` | `bool` | |
| `null / undefined` | `Option<T>` | Rust 无 null，用 Option 显式表达可选 |
| `any` | 避免使用 | 用泛型或 trait object；极端情况用 `serde_json::Value` |
| `unknown` | 泛型约束 `T: Trait` | |
| `void` | `()` | 单元类型 |
| `Promise<T>` | `impl Future<Output = T>` / `async fn` | |
| `Array<T>` | `Vec<T>` | |
| `Map<K, V>` | `HashMap<K, V>` / `BTreeMap<K, V>` | |
| `Set<T>` | `HashSet<T>` / `BTreeSet<T>` | |
| `Record<K, V>` | `HashMap<K, V>` | |
| `Buffer` / `Uint8Array` | `Vec<u8>` / `&[u8]` / `Bytes` | |

### 2.2 联合类型 → Enum

TypeScript 的联合类型在 Rust 中用 **tagged enum** 表达：

```typescript
// TypeScript
type Message = UserMessage | AssistantMessage | ToolResultMessage;
```

```rust
// Rust — 带数据的枚举（代数数据类型）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
}
```

### 2.3 接口/类型别名 → Struct

```typescript
// TypeScript
interface StreamOptions {
    temperature?: number;
    maxTokens?: number;
    apiKey?: string;
}
```

```rust
// Rust — 可选字段用 Option<T>
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamOptions {
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub api_key: Option<String>,
}
```

**原则**：
- TS 的 `?` 可选属性 → Rust 的 `Option<T>`
- TS 的 camelCase → Rust 的 snake_case（使用 `#[serde(rename_all = "camelCase")]` 兼容 JSON）
- 优先 derive `Debug, Clone, Serialize, Deserialize`

### 2.4 字面量类型 / 字符串枚举 → Rust Enum

```typescript
// TypeScript
type StopReason = "stop" | "length" | "tool_use" | "error";
```

```rust
// Rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Stop,
    Length,
    ToolUse,
    Error,
}
```

---

## 3. 面向对象 → Trait + Impl

### 3.1 接口 → Trait

```typescript
// TypeScript
interface ApiProvider {
    stream(model: Model, context: Context, options?: StreamOptions): AsyncIterable<Event>;
}
```

```rust
// Rust — 异步 trait（使用 async_trait 或 Rust 原生 RPITIT）
pub trait ApiProvider: Send + Sync {
    fn stream(
        &self,
        model: &Model,
        context: Context,
        options: Option<StreamOptions>,
    ) -> Pin<Box<dyn Stream<Item = AssistantMessageEvent> + Send + 'static>>;
}
```

### 3.2 类 → Struct + impl

```typescript
// TypeScript
class Agent {
    private tools: AgentTool[];
    constructor(config: AgentConfig) { ... }
    async run(): Promise<AgentResult> { ... }
}
```

```rust
// Rust — 不用 class，用 struct + impl
pub struct Agent {
    tools: Vec<AgentTool>,
    config: AgentConfig,
}

impl Agent {
    pub fn new(config: AgentConfig) -> Self {
        Self { tools: Vec::new(), config }
    }

    pub async fn run(&mut self) -> Result<AgentResult, AgentError> {
        // ...
    }
}
```

### 3.3 继承 → 组合 + Trait

TypeScript 的 `extends` 在 Rust 中用**组合**或 **trait 继承**：

```rust
// 组合优先
pub struct CodingAgent {
    agent: Agent,          // 内嵌基础 agent
    session: SessionManager,
}

// Trait 继承（当需要多态时）
pub trait Runnable: Send + Sync {
    async fn run(&mut self) -> Result<(), Error>;
}

pub trait CodingRunnable: Runnable {
    fn tools(&self) -> &[AgentTool];
}
```

---

## 4. 异步与并发

### 4.1 async/await

| TypeScript | Rust | 说明 |
|-----------|------|------|
| `async function` | `async fn` | 语法类似，语义不同 |
| `Promise<T>` | `impl Future<Output = T>` | Rust future 是 lazy 的 |
| `await promise` | `.await` | 后缀语法 |
| `Promise.all()` | `tokio::join!` / `futures::join_all` | |
| `Promise.race()` | `tokio::select!` | |

### 4.2 Stream（异步迭代器）

TypeScript 的 `AsyncIterable` 和事件回调 → Rust 的 `Stream` trait：

```rust
use futures::Stream;
use std::pin::Pin;

// 类型别名，简化签名
pub type AssistantMessageEventStream<'a> =
    Pin<Box<dyn Stream<Item = AssistantMessageEvent> + Send + 'a>>;
```

**注意**：Rust 的 Stream 需要 Pin，因为异步状态机可能自引用。

### 4.3 回调/钩子 → Fn trait 或 Channel

```typescript
// TypeScript — 回调
beforeToolCall?: (tool: AgentTool, args: any) => Promise<boolean>;
```

```rust
// Rust — 方式 1：Fn trait (闭包)
pub type BeforeToolCallHook = Box<dyn Fn(&AgentTool, &serde_json::Value) -> BoxFuture<'static, bool> + Send + Sync>;

// Rust — 方式 2：Channel（解耦更好）
pub type HookSender = tokio::sync::mpsc::Sender<HookEvent>;
```

---

## 5. 错误处理

### 5.1 throw → Result<T, E>

TypeScript 的 `throw` / `try-catch` → Rust 的 `Result<T, E>` + `?` 操作符：

```typescript
// TypeScript
function getModel(id: string): Model {
    const model = models.find(m => m.id === id);
    if (!model) throw new Error(`Model not found: ${id}`);
    return model;
}
```

```rust
// Rust
pub fn get_model(id: &str) -> Result<Model, ModelError> {
    models().into_iter()
        .find(|m| m.id == id)
        .ok_or_else(|| ModelError::NotFound(id.to_string()))
}
```

### 5.2 错误类型设计

每个 crate 定义自己的错误枚举，用 `thiserror` 派生：

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("Provider not found for API: {api}")]
    ProviderNotFound { api: Api },

    #[error("Tool execution failed: {name}")]
    ToolExecutionFailed { name: String, source: Box<dyn std::error::Error + Send + Sync> },

    #[error("Stream ended without result")]
    StreamEndedWithoutResult,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Http(#[from] reqwest::Error),
}
```

### 5.3 原则

- **永远不用 `unwrap()` / `panic!()`** 处理可恢复错误
- 用 `?` 传播错误
- 跨 crate 边界用 `thiserror`；应用入口用 `anyhow`
- 错误信息要有上下文（哪个 model、哪个 provider）

---

## 6. 泛型与类型约束

### 6.1 TypeScript 泛型 → Rust 泛型 + Trait Bound

```typescript
// TypeScript
function process<T extends Serializable>(item: T): string { ... }
```

```rust
// Rust
fn process<T: Serialize>(item: &T) -> String { ... }

// 复杂约束用 where 子句
fn process<T>(item: &T) -> String
where
    T: Serialize + Debug + Send + Sync,
{ ... }
```

### 6.2 鸭子类型 → 显式 Trait

TypeScript 依赖结构化子类型（鸭子类型），Rust 要求显式实现 trait：

```typescript
// TypeScript — 只要有 name 属性就行
function greet(obj: { name: string }) { ... }
```

```rust
// Rust — 必须显式声明
trait Named {
    fn name(&self) -> &str;
}

fn greet(obj: &dyn Named) { ... }
// 或泛型
fn greet<T: Named>(obj: &T) { ... }
```

---

## 7. 模块系统

### 7.1 导入导出

| TypeScript | Rust |
|-----------|------|
| `export function foo()` | `pub fn foo()` |
| `export default class` | 不存在 default export；用 `pub struct` |
| `import { foo } from './bar'` | `use crate::bar::foo;` |
| `import * as bar from './bar'` | `use crate::bar;` |
| re-export: `export { foo } from './bar'` | `pub use bar::foo;` |
| 按文件自动成为模块 | 需要在 `mod.rs` 或父模块中声明 `pub mod bar;` |

### 7.2 文件组织

```
src/
├── lib.rs          // crate 根，声明 pub mod 和 pub use
├── types.rs        // 核心类型定义
├── error.rs        // 错误类型
├── providers/
│   ├── mod.rs      // 声明子模块
│   ├── openai.rs
│   └── anthropic.rs
└── tests/
    └── mod.rs      // 集成测试
```

---

## 8. 序列化与 JSON Schema

### 8.1 @sinclair/typebox → serde + schemars

| TypeScript | Rust | 用途 |
|-----------|------|------|
| `@sinclair/typebox` | `schemars` | JSON Schema 生成 |
| `ajv` | `jsonschema` | JSON Schema 验证 |
| `JSON.parse/stringify` | `serde_json` | JSON 序列化 |
| Zod | — | Rust 用 serde 的类型系统天然保证 |

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ToolParameters {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}
```

### 8.2 工具参数 Schema

TS 中用 typebox 定义 tool schema → Rust 中用 `schemars::JsonSchema` derive 宏自动生成：

```rust
#[derive(JsonSchema, Deserialize)]
pub struct ReadFileParams {
    /// 文件路径
    pub path: String,
    /// 起始行号
    #[serde(default)]
    pub start_line: Option<u32>,
}

// 自动生成 JSON Schema
let schema = schemars::schema_for!(ReadFileParams);
```

---

## 9. 集合与迭代

### 9.1 数组方法 → Iterator 链

```typescript
// TypeScript
const names = models
    .filter(m => m.provider === "openai")
    .map(m => m.name)
    .sort();
```

```rust
// Rust — iterator 惰性求值，zero-cost abstraction
let mut names: Vec<String> = models.iter()
    .filter(|m| m.provider == Provider::OpenAI)
    .map(|m| m.name.clone())
    .collect();
names.sort();
```

### 9.2 常用转换

| TypeScript | Rust |
|-----------|------|
| `.filter()` | `.filter()` |
| `.map()` | `.map()` |
| `.find()` | `.find()` / `.iter().find()` |
| `.some()` | `.any()` |
| `.every()` | `.all()` |
| `.reduce()` | `.fold()` |
| `.flat()` | `.flatten()` |
| `.flatMap()` | `.flat_map()` |
| `.forEach()` | `.for_each()` （或用 `for` 循环） |
| `Array.from()` | `.collect::<Vec<_>>()` |
| `[...a, ...b]` | `a.into_iter().chain(b).collect()` |

---

## 10. 所有权与生命周期（Rust 独有）

### 10.1 核心原则

- **所有权**：每个值有且只有一个 owner
- **借用**：`&T`（不可变引用）或 `&mut T`（可变引用）
- **Clone vs 引用**：频繁传递的小数据（如 Provider enum）实现 `Copy`；大数据用引用或 `Arc`

### 10.2 实践策略

| 场景 | 策略 |
|------|------|
| 函数参数只读 | `&T` 借用 |
| 函数需要所有权 | `T`（move） |
| 多处共享不可变数据 | `Arc<T>` |
| 多处共享可变数据 | `Arc<RwLock<T>>` / `Arc<Mutex<T>>` |
| 配置/选项传入 | 小结构 `Clone`，大结构用引用 |
| 回调/闭包 | `Box<dyn Fn(...) + Send + Sync>` |
| 跨线程的 Stream | 确保 `Send + 'static` |

### 10.3 何时 Clone vs 引用

```rust
// ✅ 小类型：实现 Copy
#[derive(Copy, Clone)]
pub enum Provider { OpenAI, Anthropic, ... }

// ✅ 中等类型：Clone 传递
let options = stream_options.clone();

// ✅ 大类型/共享：Arc
let registry = Arc::new(ApiProviderRegistry::new());

// ❌ 避免：不必要的 clone
// 如果只需要读取，传引用
fn process(model: &Model) { ... }  // 不是 fn process(model: Model)
```

---

## 11. 测试

| TypeScript (vitest) | Rust | 说明 |
|--------------------|------|------|
| `describe` / `it` | `#[cfg(test)] mod tests` + `#[test]` | Rust 内置测试框架 |
| `expect(x).toBe(y)` | `assert_eq!(x, y)` | |
| `expect(x).toBeTruthy()` | `assert!(x)` | |
| `expect(() => ...).toThrow()` | `assert!(result.is_err())` | |
| `beforeEach` | 在每个 test fn 开头调用 setup | 或用 `rstest` 的 fixture |
| mock / spy | `mockall` crate 或手动 mock struct | |
| 异步测试 | `#[tokio::test]` | |
| `.test.ts` 文件 | `#[cfg(test)]` 模块或 `tests/` 目录 | |

---

## 12. 依赖映射

### 核心依赖

| npm Package | Rust Crate | 用途 |
|------------|------------|------|
| `@anthropic-ai/sdk` | 自行实现 HTTP 调用 | Rust 无官方 SDK，用 reqwest |
| `openai` (npm) | `async-openai` 或自行实现 | |
| `chalk` | `colored` / `owo-colors` | 终端着色 |
| `marked` | `pulldown-cmark` | Markdown 解析 |
| `ajv` | `jsonschema` | JSON Schema 验证 |
| `@sinclair/typebox` | `schemars` | JSON Schema 生成 |
| `minimatch` / `glob` | `glob` / `globset` | 文件匹配 |
| `diff` | `similar` | 文本差异比较 |
| `yaml` | `serde_yaml` | YAML 解析 |
| `proper-lockfile` | `fs2` / `fd-lock` | 文件锁 |
| `cli-highlight` | `syntect` | 语法高亮 |
| `ignore` | `ignore` (ripgrep 同作者) | gitignore 规则 |
| `extract-zip` | `zip` | ZIP 解压 |

### 异步运行时

- **运行时**: `tokio` (full features)
- **HTTP**: `reqwest`
- **Stream**: `futures` + `async-stream`
- **序列化**: `serde` + `serde_json`

---

## 13. 命名规范

| 概念 | TypeScript | Rust |
|------|-----------|------|
| 变量/函数 | camelCase | snake_case |
| 类型/结构体 | PascalCase | PascalCase |
| 常量 | UPPER_CASE | UPPER_CASE |
| 枚举成员 | PascalCase 或 UPPER_CASE | PascalCase |
| 文件名 | camelCase 或 kebab-case | snake_case |
| 包名 | kebab-case | kebab-case (Cargo) / snake_case (mod) |
| Trait | — | PascalCase + 形容词或能力词（如 `Streamable`） |
| 布尔方法 | `isX()` / `hasX()` | `is_x()` / `has_x()` |
| 构造器 | `constructor` / `new Foo()` | `Foo::new()` / `Foo::builder()` |

---

## 14. 设计模式转换

| TS 模式 | Rust 等效 | 说明 |
|---------|----------|------|
| Builder pattern (链式调用) | Builder pattern (同样适用) | Rust 中更常见，用于复杂构造 |
| Singleton | `once_cell::sync::Lazy` / `std::sync::OnceLock` | 全局延迟初始化 |
| Observer / EventEmitter | `tokio::sync::broadcast` / `tokio::sync::mpsc` | Channel 模式 |
| Registry (动态注册) | `HashMap<K, Box<dyn Trait>>` | 运行时多态 |
| Middleware / Hook | `Vec<Box<dyn Fn(...) + Send + Sync>>` | 函数对象列表 |
| Declaration merging | 不可能 | 用泛型或 enum 变体替代 |
| Optional chaining `?.` | `Option::map` / `Option::and_then` / `if let` | |

---

## 15. 不应直译的模式

以下 TypeScript 模式在 Rust 中有更好的惯用表达，**不应机械翻译**：

1. **`any` / `unknown`** → 不要用 `Box<dyn Any>`，应设计合适的枚举或泛型
2. **`null` 检查链** → 用 `Option` 的组合子 (`map`, `and_then`, `unwrap_or_default`)
3. **事件回调** → 优先用 channel (`mpsc`, `broadcast`)，而非 `Box<dyn Fn>`
4. **动态属性访问** → 用 enum 或 struct 的显式字段，避免 `HashMap<String, Value>`
5. **类继承** → 用 trait + 组合，不要模拟继承
6. **隐式类型转换** → Rust 无隐式转换，需显式 `as`, `from/into`, 或 `TryFrom`
7. **异常流控制** → 不要用 `Result` 替代异常做流程控制，`Result` 只用于真正的错误
8. **`Promise.all` + 异常** → `tokio::try_join!` 或 `futures::try_join_all`
9. **Prototype 修改** → 不存在，用 trait impl 扩展功能
10. **可变默认参数** → 用 `Default` trait + builder pattern
