# 英文词库 english.bin 的署名与许可

本目录随安装分发。`english.bin` / `english.meta.json` 是本项目的构建脚本
`scripts/build_english_dict.py` 从下列上游数据**编译派生**的二进制数据产品，
不是上游项目的原始发布物。

## 数据来源

- **wordfreq 3.1.1**，英文 `large` 词表
  - 项目主页：<https://github.com/rspeer/wordfreq>
  - wheel：`wordfreq-3.1.1-py3-none-any.whl`
  - wheel SHA-256：`4b1c6ecffc6198be3396d5cf871c4423ca71c907c231348d352dd54d62b97473`
  - 版权：Copyright 2022 Robyn Speer
  - 构建时按词表降频调用 `wordfreq.iter_wordlist("en", "large")` 与
    `wordfreq.zipf_frequency(word, "en", "large")`，不使用随机词接口。

## 许可边界（重要）

- wordfreq 的**软件**按 Apache License 2.0 发布（全文见
  [LICENSES/Apache-2.0.txt](LICENSES/Apache-2.0.txt)）。
- wordfreq 的**数据**不适用 Apache-2.0。上游 README 的「License」一节写明：
  `wordfreq` 的软件按 Apache 许可可自由再分发，而它包含的数据文件按
  Creative Commons Attribution-ShareAlike 4.0 许可再分发
  （<https://creativecommons.org/licenses/by-sa/4.0/>，全文见
  [LICENSES/CC-BY-SA-4.0.txt](LICENSES/CC-BY-SA-4.0.txt)）。
  **软件许可不覆盖数据。**
- 因此：**本项目的 MIT 许可不覆盖上游数据**。本项目自己的代码（含
  `scripts/build_english_dict.py`、`scripts/english/`、本文件与
  `data/english/overrides.tsv`）仍按本项目许可发布；`english.bin` 作为上游数据的
  派生数据产品，其数据部分继续受 CC BY-SA 4.0 及上游注明的其他署名条件约束。
- 上游在派生数据中保留的署名要求（Google Books Ngrams、Leeds Internet
  Corpus、Wikipedia、ParaCrawl、OPUS OpenSubtitles 2018、SUBTLEX-US/UK/CH/DE/NL）
  同样适用于本产品，详见 [LICENSES/README.md](LICENSES/README.md)。

## 本项目对上游数据做的修改

编译过程是确定性的，可复现；参数与来源哈希记录在 `english.meta.json`：

1. 只保留全为 ASCII 字母、长度 1..48 的 token（单字母仅保留 `a`、`i`）；
2. `key` 取 token 的 ASCII 小写；丢弃空串、数字、空白、URL、控制字符、撇号、
   连字符与其他符号，被丢弃的 token 按类别计数上报，不做变形或修补；
3. 取清洗后按词频降序排列的前 50,000 个唯一 key（不足则如实用全部，不补齐）；
4. 词频量化为百分之一 Zipf 的整数 `zipf_centi = round_half_up(Zipf × 100)`；
5. 应用 `data/english/overrides.tsv` 中**由本项目维护**的人工覆盖（词面大小写与
   少量词频修正）；
6. 为每个非空前缀建立 Top-32 引用列表，打包为 little-endian 二进制 `english.bin`。

数据是约截至 2021 年的使用快照；上游表示很可能不再更新该数据。

## 人工覆盖表

`data/english/overrides.tsv` 由本项目维护，**不是** wordfreq 的数据，也没有逐项
标注上游出处：

- 词面大小写（例如 Python、GitHub、Docker、Kubernetes、ChatGPT、I）是人工选择，
  不是上游数据里的原始形式；
- 第三列显式填写的 `zipf_centi` 是本项目的估计值（例如 350 表示 Zipf 3.50），
  不是从任何语料统计出来的出现次数；
- 没有证据的技术词沿用基础词频或使用 300～350，不设为极高频。

## 文件

| 文件 | 说明 |
| --- | --- |
| [LICENSES/Apache-2.0.txt](LICENSES/Apache-2.0.txt) | Apache License 2.0 全文（wordfreq 软件许可） |
| [LICENSES/CC-BY-SA-4.0.txt](LICENSES/CC-BY-SA-4.0.txt) | CC BY-SA 4.0 法律文本（wordfreq 数据许可） |
| [LICENSES/wordfreq-LICENSE.txt](LICENSES/wordfreq-LICENSE.txt) | wordfreq 3.1.1 随包 `LICENSE.txt` 原文 |
| [LICENSES/README.md](LICENSES/README.md) | 许可适用范围与上游许可声明原文摘录 |
| [overrides.tsv](overrides.tsv) | 本项目维护的人工覆盖表 |
