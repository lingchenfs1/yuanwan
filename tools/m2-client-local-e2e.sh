#!/usr/bin/env bash
# M2a 本机回环端到端：fakegw（yw-serve 的真服务代码回放录像）→ yw-client 无界面模式，不碰 PS5、不碰网关机。
#
# 用法：tools/m2-client-local-e2e.sh --clip <录像.h265> [选项]
#   --clip <文件>      M1 record 或 m0-probe --dump 录的 Annex-B 录像（开头带参数集）
#   --fakegw <路径>    默认 ${CARGO_TARGET_DIR:-target}/release/fakegw
#   --client <路径>    默认 ${CARGO_TARGET_DIR:-target}/release/yw-client
#   --port <端口>      fakegw 监听 127.0.0.1 的哪个端口，默认随机挑一个 20000~59999
#   --work <目录>      工作目录（必须不存在或为空），默认在 $TMPDIR 下新建
#   --duration <秒>    长连那一次的时长，默认 20
#   --runs <次数>      连续连接测试的次数，默认 20（传给 m2-client-connect100.sh -n）
#   --run-secs <秒>    连续连接测试每次连多久，默认 3
#
# 步骤：
#   1. 在工作目录里建网关密钥（fakegw keygen）和客户端密钥（yw-client keygen，YW_CONFIG_DIR 指到工作目录），
#      配对串经管道从 fakegw pair add --stdout 直接交给 yw-client pair import（不落盘、不显示）；
#      客户端档案不设 bind_if（回环地址不能绑物理网卡）；
#   2. 起 fakegw（--loop），等它输出 listening；
#   3. yw-client --headless --connect-once --duration <秒> --json 连一次，
#      要求 ok=true、first_frame_ms ≤ 2000、frames ≥ 50 × 时长、freezes = 0；
#   4. tools/m2-client-connect100.sh 连 <次数> 次，要求全部成功；
#   5. 放停止文件让 fakegw 退出（3 s 不退再按 pid 结束），打印它的 summary；在所有日志里 grep 私钥、reset_key、PSK（不打印它们），一条都不能有。
# 工作目录里有测试用的密钥，脚本不删，用完请自行清理。
# 退出码：全部通过 0；有不通过 1；参数或环境不对 2。

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
TGT="${CARGO_TARGET_DIR:-${HERE}/../target}"
CLIP=""
FAKEGW="${TGT}/release/fakegw"
CLIENT="${TGT}/release/yw-client"
PORT=""
WORK=""
DUR=20
RUNS=20
RUN_SECS=3

usage() {
    # 打印文件开头的注释块（第 2 行起，到第一行不是注释的为止）
    awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --clip) CLIP="${2:-}"; shift 2 ;;
        --fakegw) FAKEGW="${2:-}"; shift 2 ;;
        --client) CLIENT="${2:-}"; shift 2 ;;
        --port) PORT="${2:-}"; shift 2 ;;
        --work) WORK="${2:-}"; shift 2 ;;
        --duration) DUR="${2:-}"; shift 2 ;;
        --runs) RUNS="${2:-}"; shift 2 ;;
        --run-secs) RUN_SECS="${2:-}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "不认识的参数：$1" >&2; usage >&2; exit 2 ;;
    esac
done

for v in "$DUR" "$RUNS" "$RUN_SECS"; do
    case "$v" in ''|*[!0-9]*) echo "--duration、--runs、--run-secs 要正整数" >&2; exit 2 ;; esac
done
[ -n "$CLIP" ] && [ -f "$CLIP" ] || { echo "要 --clip <录像.h265>" >&2; exit 2; }
[ -x "$FAKEGW" ] || { echo "找不到 fakegw：${FAKEGW}（先 cargo build --release -p yw-sim）" >&2; exit 2; }
[ -x "$CLIENT" ] || { echo "找不到 yw-client：${CLIENT}（先 cargo build --release -p yw-client）" >&2; exit 2; }
if [ -z "$PORT" ]; then
    PORT=$((20000 + RANDOM % 40000))
fi
if [ -z "$WORK" ]; then
    WORK="$(mktemp -d "${TMPDIR:-/tmp}/m2-e2e.XXXXXX")"
elif [ -e "$WORK" ] && [ -n "$(ls -A "$WORK")" ]; then
    echo "工作目录 ${WORK} 不是空的" >&2
    exit 2
fi
mkdir -p "$WORK"
chmod 700 "$WORK"
export YW_CONFIG_DIR="${WORK}/client"
KEYS="${WORK}/keys"
PIDF="${WORK}/fakegw.pid"

STOPF="${WORK}/fakegw.stop"

# 先放停止文件让 fakegw 自己退出（发 CLOSE(5)、输出 summary）；3 s 不退再按 pid 发 TERM，最后才 KILL
wait_gone() {
    local pid="$1" n="$2"
    while [ "$n" -gt 0 ]; do
        kill -0 "$pid" 2>/dev/null || return 0
        sleep 0.2
        n=$((n - 1))
    done
    return 1
}
stop_fakegw() {
    [ -f "$PIDF" ] || return 0
    local pid
    pid="$(cat "$PIDF")"
    touch "$STOPF"
    if ! wait_gone "$pid" 15; then
        echo "fakegw（pid ${pid}）3 s 没退出，发 TERM" >&2
        kill "$pid" 2>/dev/null || true
        if ! wait_gone "$pid" 10; then
            echo "fakegw（pid ${pid}）还在，强制结束" >&2
            kill -9 "$pid" 2>/dev/null || true
        fi
    fi
    wait "$pid" 2>/dev/null || true
    mv "$PIDF" "${PIDF}.done"
}
trap stop_fakegw EXIT

echo "== 1. 密钥和配对（工作目录 ${WORK}）" >&2
"$CLIENT" keygen >/dev/null
CPUB="$("$CLIENT" pubkey)"
"$FAKEGW" keygen --keys "$KEYS"
"$FAKEGW" pair add e2e "$CPUB" --keys "$KEYS" --entry "127.0.0.1:${PORT}" --stdout \
    | "$CLIENT" pair import --name local >&2

echo "== 2. 起 fakegw：127.0.0.1:${PORT}" >&2
"$FAKEGW" --keys "$KEYS" --listen "127.0.0.1:${PORT}" --file "$CLIP" --loop --stop-file "$STOPF" \
    >"${WORK}/fakegw.out" 2>"${WORK}/fakegw.log" </dev/null &
echo $! >"$PIDF"
ready=0
for _ in $(seq 1 50); do
    if grep -q '"event":"listening"' "${WORK}/fakegw.out" 2>/dev/null; then
        ready=1
        break
    fi
    kill -0 "$(cat "$PIDF")" 2>/dev/null || break
    sleep 0.1
done
if [ "$ready" != 1 ]; then
    echo "fakegw 没起来，日志：${WORK}/fakegw.log" >&2
    exit 1
fi

pass=1
echo "== 3. 长连一次：${DUR} s" >&2
rc=0
"$CLIENT" --headless --connect-once --duration "$DUR" --json --profile local \
    >"${WORK}/headless.out" 2>"${WORK}/headless.log" </dev/null || rc=$?
HL="$(tail -n 1 "${WORK}/headless.out" 2>/dev/null || true)"
printf '%s\n' "$HL"
MIN_FRAMES=$((DUR * 50))
CHECK="$(printf '%s\n' "$HL" | awk -v minf="$MIN_FRAMES" '
function num(s, key,    pat) {
    pat = "\"" key "\":-?[0-9.]+"
    if (match(s, pat)) return substr(s, RSTART + length(key) + 3, RLENGTH - length(key) - 3)
    return ""
}
{
    bad = ""
    if ($0 !~ /"ok":true/) bad = bad " ok!=true"
    f = num($0, "first_frame_ms"); if (f == "" || f + 0 > 2000) bad = bad " first_frame_ms=" (f == "" ? "null" : f)
    n = num($0, "frames"); if (n == "" || n + 0 < minf) bad = bad " frames=" n "<" minf
    z = num($0, "freezes"); if (z == "" || z + 0 != 0) bad = bad " freezes=" z
    print (bad == "" ? "ok" : bad)
}')"
if [ "$rc" != 0 ] || [ "$CHECK" != ok ]; then
    echo "长连不通过（rc=${rc}）：${CHECK:-没有输出}；日志 ${WORK}/headless.log" >&2
    pass=0
else
    echo "长连通过" >&2
fi

echo "== 4. 连续连接 ${RUNS} 次，每次 ${RUN_SECS} s" >&2
rc=0
"${HERE}/m2-client-connect100.sh" -n "$RUNS" -d "$RUN_SECS" -p local -c "$CLIENT" -o "${WORK}/connect" -g 0.5 \
    >"${WORK}/connect.out" || rc=$?
tail -n 1 "${WORK}/connect.out"
if [ "$rc" != 0 ]; then
    echo "连续连接不通过（rc=${rc}），明细 ${WORK}/connect/connect.jsonl" >&2
    pass=0
fi

echo "== 5. 结束 fakegw，查日志脱敏" >&2
stop_fakegw
tail -n 1 "${WORK}/fakegw.out"
PAT="${WORK}/.secrets"
(
    umask 077
    {
        grep -v '^#' "${KEYS}/gateway.key"
        grep -v '^#' "${KEYS}/reset.key"
        awk '!/^#/ && NF >= 3 { print $3 }' "${KEYS}/clients"
        sed -n 's/^secret=//p' "${YW_CONFIG_DIR}/client.key"
    } | tr -d ' \r' | grep -E '^[0-9A-Fa-f]{64}$' >"$PAT"
)
nsec="$(wc -l <"$PAT" | tr -d ' ')"
LOGS=("${WORK}/fakegw.out" "${WORK}/fakegw.log" "${WORK}/headless.out" "${WORK}/headless.log" "${WORK}/connect.out")
for f in "${WORK}/connect/"*; do
    LOGS+=("$f")
done
leaks="$(grep -l -i -F -f "$PAT" "${LOGS[@]}" 2>/dev/null || true)"
if [ "$nsec" -lt 4 ]; then
    echo "脱敏检查：只取到 ${nsec} 个密钥（应为 4 个），检查不完整" >&2
    pass=0
elif [ -n "$leaks" ]; then
    echo "脱敏检查不通过：以下日志里出现了密钥或 PSK：" >&2
    printf '  %s\n' "$leaks" >&2
    pass=0
else
    echo "脱敏检查通过：${#LOGS[@]} 个日志文件里没有私钥、reset_key、PSK" >&2
fi

if [ "$pass" = 1 ]; then
    echo "全部通过。工作目录 ${WORK}（含测试密钥，用完请清理）" >&2
    exit 0
fi
echo "有不通过的项。工作目录 ${WORK}" >&2
exit 1
