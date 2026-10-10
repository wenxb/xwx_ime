#!/bin/bash
# 输入法冻住的那一刻，拍一张现场快照。
#
# 为什么需要它：引擎的所有层都能量到——库内计算 ≤56ms、D-Bus 往返 ≤1.15ms、
# 没有内存/磁盘/构建问题、ibus-daemon 7 天零自发崩溃。唯一测不到的是
# **GNOME Shell 面板那一环**：引擎给面板发信号，对端不消费就会反压，而
# `ProcessKeyEvent` 是同步方法，它不返回，用户看到的就是卡死、键还会被
# ibus 超时扔掉（丢字）。
#
# 要证实/证伪它，只需要在**冻住的那一刻**看引擎的内核栈：如果卡在
# `unix_stream_sendmsg` 一类的地方，就是反压实锤；如果卡在 futex，是锁；
# 如果栈里是排序/查词典，才是引擎自己在算。
#
# 这**只是读**，不改任何东西、不杀进程、不重启输入法。卡的时候跑一下：
#
#   scripts/freeze_snapshot.sh
#
# 输出写到 stdout（可重定向存档）。趁它还冻着跑，恢复后再跑就没用了。

set -u

ENGINE=$(pgrep -f "xwx_ime --ibus" | head -1)
if [ -z "$ENGINE" ]; then
    echo "!! 没找到 xwx_ime 进程——输入法此刻根本没在跑（那就是另一回事了）"
    exit 1
fi

echo "=== xwx_ime 冻结快照  $(date '+%F %T')  pid=$ENGINE ==="
echo
echo "--- 进程状态（D=不可中断睡眠=等 IO；R=在算；S=在等）---"
awk '{print "  state="$3"  utime="$14"  stime="$15"  majflt="$12}' "/proc/$ENGINE/stat"
echo
echo "--- 内核栈（wchan：卡在哪个内核函数上）---"
echo "  wchan = $(cat "/proc/$ENGINE/wchan" 2>/dev/null)"
echo "  /proc/$ENGINE/stack:"
if [ -r "/proc/$ENGINE/stack" ]; then
    sudo -n cat "/proc/$ENGINE/stack" 2>/dev/null | head -12 | sed 's/^/    /' ||
        head -12 "/proc/$ENGINE/stack" 2>/dev/null | sed 's/^/    /' ||
        echo "    （读 stack 需要权限；上面的 wchan 已经足够定位）"
else
    echo "    （不可读；wchan 一行已足够定位）"
fi
echo
echo "--- 每个线程在干嘛（找出卡住的那一个）---"
for t in "/proc/$ENGINE/task/"*; do
    tid=$(basename "$t")
    s=$(awk '{print $3}' "$t/stat" 2>/dev/null)
    w=$(cat "$t/wchan" 2>/dev/null)
    printf "  tid=%-8s state=%-3s wchan=%s\n" "$tid" "$s" "$w"
done
echo
echo "--- 用户态栈（最直接：卡在哪一行）---"
if command -v gdb >/dev/null 2>&1; then
    timeout 10 gdb -p "$ENGINE" -batch \
        -ex "thread apply all bt 8" 2>/dev/null | head -60 ||
        echo "  （gdb 附加失败——上面的内核栈/wchan 仍然有效）"
else
    echo "  （没装 gdb；上面的 wchan/线程状态已足够判断卡在哪一类）"
fi
echo
echo "--- 当时系统在忙什么 ---"
echo "  load: $(cut -d' ' -f1-3 /proc/loadavg)"
echo "  内存: $(grep -E 'MemAvailable' /proc/meminfo | awk '{print $2" kB 可用"}')"
echo "  gnome-shell: pid=$(pgrep -x gnome-shell | head -1)  cpu=$(ps -o %cpu= -p "$(pgrep -x gnome-shell | head -1)" 2>/dev/null)"
echo "  ibus-daemon: pid=$(pgrep -x ibus-daemon | head -1)  cpu=$(%cpu= true 2>/dev/null; ps -o %cpu= -p "$(pgrep -x ibus-daemon | head -1)" 2>/dev/null)"
