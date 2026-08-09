# rime_oracle — librime (luna_pinyin) oracle for xwx_ime

对照工具：直接驱动本机已装的 librime 1.10.0（`luna_pinyin` 方案），
把拼音串喂进去，按固定协议输出 preedit、候选总数、前 10 个候选，
用于和 `xwx_ime` 引擎的输出逐条比对。

## 编译

```sh
cd tools/rime_oracle
./build.sh
```

只有一条 `g++` 命令，直接链接 `/usr/lib/x86_64-linux-gnu/librime.so.1`。
头文件 `vendor/rime_api.h` 是从 librime 1.10.0 源码取的（与本机 so 版本对应）。

## 运行

```sh
./rime_oracle < ../corpus/basic.txt
```

- stdin 每行一个 ASCII 拼音串；空行和 `#` 开头的行会被跳过。
- 每个输入使用独立 session（create → simulate → get_context → 全量迭代
  candidate list → destroy），互不影响。

## 首次运行会部署词典（几十秒，正常现象）

第一次跑时 librime 会把 `luna_pinyin` 词典编译进用户目录，
可能花几十秒，之后再次运行就很快。这是 librime 的正常部署流程，
不是卡住。若想重跑首次部署，删掉 `.rime_user/` 即可。

## user_data_dir 在哪

`tools/rime_oracle/.rime_user/`（绝对路径写死在源码里）。

**绝不**指向 `~/.local/share/fcitx5/rime`——那是日常在用的输入法配置。
该目录已被 `.gitignore` 忽略，编译产物 `rime_oracle` 同样被忽略。

## 输出协议

逐字节遵守，供对端（xwx_ime 侧）解析比对：

```
IN\t<原始输入串>
PREEDIT\t<preedit 文本；没有就是空串，但这一行必须在>
NCAND\t<候选总数（candidate_list 全量迭代计数，不是当前页）>
CAND\t1\t<候选文本>
CAND\t2\t<候选文本>
...（最多 10 条）
END
```

`END` 之后没有空行，下一条记录直接跟 `IN`。
