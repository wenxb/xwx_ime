# 打字模拟评测报告（自动聚合）


## 词级（简拼/混合，覆盖 dict 抽样）

样本 494930 条，按 style：
- `abbrev`：60894 条，召回 99.1%，首选 27.2%，首屏 54.2%（词频加权：召回 99.4%，首选 28.6%）
- `abbrev_unfold`：25867 条，召回 99.5%，首选 34.4%，首屏 61.5%（词频加权：召回 99.8%，首选 35.3%）
- `full`：9876 条，召回 99.9%，首选 96.1%，首屏 99.9%（词频加权：召回 100.0%，首选 97.3%）
- `mixed`：382490 条，召回 98.8%，首选 73.6%，首屏 93.1%（词频加权：召回 98.7%，首选 76.8%）
- `mixed_unfold`：15803 条，召回 98.7%，首选 50.5%，首屏 83.6%（词频加权：召回 97.6%，首选 56.0%）
- `abbrev` 按词长（词字符数）：
  - 2 字：24974 条，召回 97.8%，首选 1.7%，首屏 14.2%
  - 3 字：15056 条，召回 99.9%，首选 21.3%，首屏 67.0%
  - 4 字：16450 条，召回 100.0%，首选 55.1%，首屏 91.0%
  - 5 字：4414 条，召回 100.0%，首选 87.0%，首屏 99.3%
- `mixed` 按词长（词字符数）：
  - 2 字：49844 条，召回 97.9%，首选 18.0%，首屏 67.3%
  - 3 字：90138 条，召回 98.4%，首选 66.8%，首屏 94.2%
  - 4 字：229586 条，召回 99.1%，首选 87.2%，首屏 97.9%
  - 5 字：12922 条，召回 99.4%，首选 94.1%，首屏 98.4%
- zh/ch/sh 双写法对照（60894 词都有两种样本）：折叠召回失败后全声母召回成功 120；全声母失败而折叠成功 0
- 混合缩写按模式形状（音节数×头/尾/交错/纯简拼）：
  - 2 音节 head：29009 条，召回 97.8%
  - 2 音节 tail：24924 条，召回 97.7%
  - 3 音节 head：34365 条，召回 98.6%
  - 3 音节 interior：30046 条，召回 98.6%
  - 3 音节 tail：30070 条，召回 98.2%
  - 4 音节 head：56566 条，召回 99.2%
  - 4 音节 interior：131192 条，召回 99.1%
  - 4 音节 tail：49199 条，召回 99.2%
  - 5 音节 head：941 条，召回 99.3%
  - 5 音节 interior：5409 条，召回 99.2%
  - 5 音节 tail：867 条，召回 99.0%
  - 6 音节 head：286 条，召回 99.7%
  - 6 音节 interior：2981 条，召回 99.6%
  - 6 音节 tail：326 条，召回 100.0%
  - 7 音节 head：44 条，召回 100.0%
  - 7 音节 interior：1395 条，召回 99.9%
  - 7 音节 tail：76 条，召回 100.0%
  - 8 音节 head：22 条，召回 100.0%
  - 8 音节 interior：563 条，召回 100.0%
  - 8 音节 tail：12 条，召回 100.0%

未召回共 5526 条；词频最高的 30 条：
  - `eshouche` → 「二手车」（mixed）top3: 饿瘦成/扼守/额手
  - `diang` → 「电工」（mixed）top3: 迪昂/地/第
  - `jingcankao` → 「仅供参考」（mixed）top3: 经参考/经/惊
  - `shao` → 「是好」（mixed_unfold）top3: 少/烧/稍
  - `sa` → 「手按」（abbrev）top3: 撒/洒/飒
  - `sha` → 「手按」（abbrev_unfold）top3: 啥/傻/杀
  - `shan` → 「手按」（mixed）top3: 山/删/闪
  - `san` → 「手按」（mixed_unfold）top3: 三/散/伞
  - `shoua` → 「手按」（mixed）top3: 手啊/手/受
  - `nang` → 「难过」（mixed）top3: 囊/囔/馕
  - `youguangding` → 「有关规定」（mixed）top3: 有光腚/有光/油光
  - `elou` → 「二楼」（mixed）top3: 饿楼/饿/呃
  - `qin` → 「七年」（mixed）top3: 亲/秦/琴
  - `zhun` → 「逐年」（mixed）top3: 准/谆/肫
  - `shou` → 「事后」（mixed_unfold）top3: 手/受/收
  - `chanpingge` → 「产品规格」（mixed）top3: 铲平/产品给/产品更
  - `xin` → 「细腻」（mixed）top3: 新/心/信
  - `chui` → 「参会」（mixed）top3: 吹/垂/锤
  - `eshi` → 「而是」（mixed）top3: 俄式/恶事/饿时
  - `quanggedi` → 「全国各地」（mixed）top3: 全割掉/去/取
  - `sa` → 「涉案」（abbrev）top3: 撒/洒/飒
  - `san` → 「涉案」（mixed_unfold）top3: 三/散/伞
  - `sha` → 「涉案」（abbrev_unfold）top3: 啥/傻/杀
  - `shan` → 「涉案」（mixed）top3: 山/删/闪
  - `shea` → 「涉案」（mixed）top3: 射啊/蛇啊/射
  - `ahu` → 「爱护」（mixed）top3: 阿华/阿虎/阿花
  - `dezhang` → 「第二章」（mixed）top3: 德章/的/地
  - `diezhang` → 「第二章」（mixed）top3: 叠嶂/跌涨/爹
  - `chong` → 「彩虹」（mixed）top3: 冲/充/重
  - `kenzai` → 「可能在」（mixed）top3: 肯在/肯/恳

慢查询（>12ms）79242 条，最慢 10 条：
  - `chjiazhixiao` ms_max=143.8 slow=lookup|143.8
  - `shijianzhz` ms_max=96.5 slow=lookup|96.5
  - `zxianshiting` ms_max=85.2 slow=lookup|85.2
  - `chanquanzhid` ms_max=80.6 slow=lookup|80.6
  - `zhxiqshu` ms_max=78.3 slow=lookup|78.3
  - `zhongyangzhye` ms_max=77.7 slow=lookup|77.7
  - `khechengji` ms_max=71.1 slow=lookup|71.1
  - `kongjshji` ms_max=71.0 slow=lookup|71.0
  - `gsijingli` ms_max=68.5 slow=lookup|68.5
  - `gaishanhuanzhjiuytiy` ms_max=68.4 slow=lookup|68.4

### 词级问题清单（前若干）

- 未召回 `eshouche`（mixed）→ 「二手车」，top3 = 饿瘦成/扼守/额手
- 未召回 `diang`（mixed）→ 「电工」，top3 = 迪昂/地/第
- 未召回 `jingcankao`（mixed）→ 「仅供参考」，top3 = 经参考/经/惊
- 未召回 `shao`（mixed_unfold）→ 「是好」，top3 = 少/烧/稍
- 未召回 `sa`（abbrev）→ 「手按」，top3 = 撒/洒/飒
- 未召回 `sha`（abbrev_unfold）→ 「手按」，top3 = 啥/傻/杀
- 未召回 `shan`（mixed）→ 「手按」，top3 = 山/删/闪
- 未召回 `san`（mixed_unfold）→ 「手按」，top3 = 三/散/伞
- 未召回 `shoua`（mixed）→ 「手按」，top3 = 手啊/手/受
- 未召回 `nang`（mixed）→ 「难过」，top3 = 囊/囔/馕
- 未召回 `youguangding`（mixed）→ 「有关规定」，top3 = 有光腚/有光/油光
- 未召回 `elou`（mixed）→ 「二楼」，top3 = 饿楼/饿/呃
- 未召回 `qin`（mixed）→ 「七年」，top3 = 亲/秦/琴
- 未召回 `zhun`（mixed）→ 「逐年」，top3 = 准/谆/肫
- 未召回 `shou`（mixed_unfold）→ 「事后」，top3 = 手/受/收

## 会话级（人民日报 1998-01 句子打字模拟）

词事件 132276 个，选字结果：
- `space`：102228（77.3%）
- `select`：22604（17.1%）
- `select10`：401（0.3%）
- `deep_fallback`：1181（0.9%）
- `leftover`：97（0.1%）
- `no_cand`：1585（1.2%）
- `full_retry`：3772（2.9%）
- `retry_fail`：408（0.3%）
前缀抖动：lost=12（0.0%），fs_churn=9（0.0%）
句末标点：{'ok': 3172}
按 style：
- `abbrev`：15341 事件，首选直上 38.0%，数字选词 34.9%，深排名 0.0%，无候选 0.0%，残留 0.0%，重打全拼 24.5%
- `full`：116622 事件，首选直上 82.4%，数字选词 15.1%，深排名 1.0%，无候选 1.4%，残留 0.1%，重打全拼 0.0%
- `mixed`：313 事件，首选直上 89.5%，数字选词 4.5%，深排名 0.0%，无候选 0.0%，残留 0.0%，重打全拼 2.2%

深排名（≥10）1181 个，名次最深的 15 个：
  - `ji` → 「暨」rank=78
  - `yi` → 「彝」rank=66
  - `xi` → 「郗」rank=51
  - `yu` → 「禺」rank=51
  - `li` → 「莅」rank=48
  - `zhi` → 「栀」rank=48
  - `shi` → 「蚀」rank=44
  - `yu` → 「渝」rank=43
  - `jian` → 「翦」rank=43
  - `yan` → 「闫」rank=42
  - `xi` → 「奚」rank=39
  - `yu` → 「逾」rank=38
  - `yu` → 「逾」rank=38
  - `yu` → 「逾」rank=38
  - `yu` → 「逾」rank=38
无候选 1585 个，前 15：
  - `xianci` → 「献辞」（full）
  - `rujun` → 「如钧」（full）
  - `qinghong` → 「庆红」（full）
  - `zemin` → 「泽民」（full）
  - `rongji` → 「镕基」（full）
  - `ruihuan` → 「瑞环」（full）
  - `huaqing` → 「华清」（full）
  - `jianxing` → 「健行」（full）
  - `zemin` → 「泽民」（full）
  - `rongji` → 「镕基」（full）
  - `ruihuan` → 「瑞环」（full）
  - `jianxing` → 「健行」（full）
  - `lanqing` → 「岚清」（full）
  - `jiazheng` → 「加正」（full）
  - `zemin` → 「泽民」（full）

整句零干预（485 句）：整句完全正确 36.3%，平均字转换相似度 0.760
- 击键：实际 13802 键 / 全拼基准 15699 键 ⇒ 平均每字 3.22 键，击键节省率 12.1%
最差 10 句：
  - 相似 0.00 [19980101-03-001-002:whole] 期望 周德武… / 实得 轴的无…
  - 相似 0.00 [19980101-07-002-002:whole] 期望 李瑛… / 实得 理应…
  - 相似 0.00 [19980103-03-020-001:whole] 期望 据… / 实得 巨…
  - 相似 0.00 [19980105-06-015-001:whole] 期望 据… / 实得 巨…
  - 相似 0.00 [19980105-09-004-008:whole] 期望 龚德俊是… / 实得 公的军事…
  - 相似 0.00 [19980105-11-010-002:whole] 期望 陶立华王力… / 实得 桃李花往里…
  - 相似 0.00 [19980106-09-001-017:whole] 期望 底… / 实得 地…
  - 相似 0.00 [19980106-09-003-002:whole] 期望 刘杰… / 实得 六届…
  - 相似 0.00 [19980106-11-013-002:whole] 期望 杨宝珍… / 实得 养保证）…
  - 相似 0.00 [19980106-12-007-002:whole] 期望 卢永华… / 实得 录用话…

词事件耗时 p50/p95：6.26/36.55 ms；估算逐键 p50/p95：1.19/7.09 ms；最慢键超 12ms 的词事件 22921 个（17.3%）
  - `shiji` → 「实际」ms_max=60.8
  - `zhuyi` → 「注意」ms_max=59.3
  - `shuochangdaoduan` → 「说长道短」ms_max=58.8
  - `zy` → 「资源」ms_max=58.4
  - `jingji` → 「经济」ms_max=58.4
  - `llshk` → 「琅琅上口」ms_max=58.4
  - `zhe` → 「这」ms_max=57.1
  - `zhou` → 「周」ms_max=56.1
  - `shengzan` → 「盛赞」ms_max=52.5
  - `zhuanjia` → 「专家」ms_max=50.3
