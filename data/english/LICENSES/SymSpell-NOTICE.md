# 英文二元词频数据（english_bigram.bin）的来源与条款

本文件记录 **M2 静态二元数据**的来源、加工方式与许可，供分发时随包附带
（`Cargo.toml` 把它装到 `/usr/share/doc/xwx-ime/english-bigram-NOTICE.md`）。

## 数据来源

- 上游仓库：<https://github.com/wolfgarbe/SymSpell>
- 固定提交：`736034a4f0436da6a98c94f83f54ec6de2ec25c6`
- 文件：`SymSpell/frequency_bigramdictionary_en_243_342.txt`
- 下载：<https://raw.githubusercontent.com/wolfgarbe/SymSpell/736034a4f0436da6a98c94f83f54ec6de2ec25c6/SymSpell/frequency_bigramdictionary_en_243_342.txt>
- SHA-256：`fd892a160184101dd7ae807ac5a302d01fcea1c47304181a8ed7ed9c94545bcd`
- 实际字节数：5,137,213；实际行数：242,342（**不按文件名猜行数**）

已核对的上游数据示例：

```
hello world 7852480
looking forward 271821504
thank you 733376704
```

## 加工方式

本仓库**只读取词表数据**，不运行 SymSpell 的纠错算法、不引入它的代码。
处理脚本：`scripts/build_english_bigram.py`（布局在 `scripts/english/bigram.py`）。

1. 只保留 prev/next 两个词**都在构建时英文词库**（`data/english.bin`）里的词对；
2. 键按 M1 英文 key 规则校验并转小写，归一化后重复词对用 checked_add 合并；
3. 每个 prev 按 count 降序、next key 升序保留 **Top-32**；
4. 产物是 little-endian 二进制：64 字节头 → context(24B) → edge(16B) → strings。

构建时必须记录被引用的 `english.bin` 的 SHA-256（写进
`english_bigram.meta.json`）：词库换了以后整份二元数据要重新构建并重跑验收。

## 许可与条款

- **SymSpell 仓库的代码许可是 MIT**。但 `english.bin`（wordfreq 数据）是
  CC BY-SA 4.0，而本二元表是从 SymSpell 的数据文件编译来的——**上游代码的
  MIT 不能代替对数据来源条款的记录**，所以本文件单独存在。
- 本数据是**静态、偏书面语**的词对频率表（来源以英文书面语料为主），
  不代表现代聊天或技术英语的最优模型，也不宣称覆盖完整的语言概率分布。
  本仓库只把它当作**供候选排序使用的平滑频率估计**。
- 若上游数据的实际条款与本说明不符，以**上游仓库的说明**为准；本仓库
  不额外主张任何权利，也不对数据本身做再许可。
- 该文件是**可选**的：缺失时 XWX 只关闭静态条件排序与零输入下一词预测，
  英文前缀补全、中文输入与用户学习全部照常。
