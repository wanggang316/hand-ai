#### 1. Model Generation

- **二进制**: `src/bin/generate_models.rs`
- 从 OpenRouter、Vercel AI Gateway、models.dev 拉取模型列表，合并并去重后写入 `src/models.json`
- 运行: `cargo run --bin generate_models`
- 库通过 `model::models::models()` 或 `include_str!("models.json")` 使用生成的 JSON
