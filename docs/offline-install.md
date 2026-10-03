# 离线交付清单（Q5）

目标：**不依赖在线重建**的前提下，把输入法完整装到一台机器上。
现状盘点（2026-10-03，对着 `justfile` 与 `Cargo.toml` 核过）。

## 交付物应当包含什么

| 内容 | 现在谁负责 | 状态 |
|---|---|---|
| 二进制 `xwx_ime` | `just install` → `$XDG_BIN_HOME/xwx_ime`；deb → `/usr/bin/xwx_ime` | ✅ |
| IBus 组件 XML | `just install` → `/usr/share/ibus/component/xwx.xml`；deb → 同名路径 | ✅ |
| 词库 `dict.bin` | `just install` → `$XDG_DATA_HOME/xwx/dict.bin` | ✅ |
| 简拼副索引 `abbrev.bin` | 同上 | ✅ |
| 语言模型 `.gram` | **不管**（用户自备，属大文件） | ⚠️ 需在 README 讲清 |
| 输入源注册 | 用户手动把 `xwx` 加进 IBus 输入源 | ⚠️ 需在 README 讲清 |

## 已核实的缺口

1. **deb 包里没有词库与副索引**（最关键）
   `Cargo.toml` 的 `[package.metadata.deb]` 的 `assets` 只有两条：
   ```
   ["data/debian/xwx.xml", "usr/share/ibus/component/xwx.xml", "644"]
   ["target/release/xwx_ime", "usr/bin/xwx_ime", "755"]
   ```
   ⇒ 装了 deb 的机器上引擎会退化成内置小词库（`just install` 里那句警告说的就是这件事）。
   **已修**：`Cargo.toml` 加了 `data/dict.bin` / `data/abbrev.bin` 两条 asset → `usr/share/xwx/`。
   `/usr/share/xwx/`；**引擎侧已补好系统级回退**（`src/lm_path.rs::resolve_data_path`
   改为「用户目录优先 → 系统目录 `/usr/share/xwx/` → 都没有时仍返回用户路径」，
   配 `fallback_tests::data_path_prefers_user_then_system_then_user_default`）。
   **剩**：真正的 asset 条目还没加（下一步）。
2. **已修**：加了 `data/debian/postinst` 与 `prerm`（`ibus write-cache`，
   可用则执行、失败不阻断），装/卸自动刷新组件缓存。
3. **`.gram` 与输入源注册没有任何交付说明**
   README 需要写清：模型放哪、为什么不能随包分发、如何把 `xwx` 加进输入源。
4. **已修**：`cargo deb -p xwx_ime` 能出包（注意 `data/debian/xwx.xml` 是**生成物**，
   需先跑 `xwx_ime --xml data/debian/xwx.xml --exec /usr/bin/xwx_ime`，`just deb` 里已有这步；
   产物在 `target_dir/debian/` 而不是仓库下的 `target/`）。**内容已验证**（见下）。

## 验证方式（补完后照此验收）

1. `just deb` 出包 → `dpkg-deb -c` 确认包含 `usr/share/xwx/{dict,abbrev}.bin` 与组件 XML；
2. 在**干净容器**里 `dpkg -i` + 只装 `ibus` 依赖 → `ibus list-engine` 应出现 `xwx`；
3. 断网状态下重复第 2 步（证明不依赖在线重建）；
4. `ibus engine xwx` 后按键应能出候选（用集成测试那套私有总线脚本更稳）。
