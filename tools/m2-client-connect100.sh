#!/usr/bin/env bash
# M2a 验收（T22、T25）：yw-client 无界面模式连续连接、断开 N 次（默认 100），
# 记下每次用了几次 HS_INIT、出首帧用了多久，最后汇总成功率。网关、fakegw 都能当对端。
#
# 用法：tools/m2-client-connect100.sh [-n 次数] [-d 每次秒数] [-p 档案名] [-c yw-client] [-o 结果目录] [-g 间隔秒] [-s]
#   -n  连几次，默认 100
#   -d  每次连多久（秒，传给 --duration），默认 4
#   -p  客户端档案名（yw-client pair list 里的名字）；不给时由 yw-client 自己选（只有一个档案时）
#   -c  yw-client 可执行文件；默认用环境变量 YW_CLIENT，再不然用 PATH 里的 yw-client
#   -o  结果目录（不存在就建）：connect.jsonl（每次一行）、run-<i>.out / run-<i>.log（每次的 stdout / stderr）、
#       summary.json；默认在 $TMPDIR 下新建一个
#   -g  两次之间等几秒（可以带小数），默认 1
#   -s  连上后发 PS5_CMD START（yw-client --start）
#   环境变量 YW_CONFIG_DIR 照常传给 yw-client（换配置目录）。
#
# stdout：每次一行 JSON，在 yw-client 输出的那一行前面加上 i、rc、wall_ms；最后一行是汇总 JSON（带 "summary":true）。
# stderr：进度和人读的汇总。
# 退出码：全部成功 0；有失败 1；参数不对 2。
#
# 失败时先看 run-<i>.log：fail_reason 为 no_hs_resp（握手没回音，多半是丢包或地址不通）、
# no_data（回了 HS_RESP 但 DATA 不通）、rejected:*（被网关拒绝）、closed:*（被网关关掉）、no_json（yw-client 没输出结果）。

set -euo pipefail

N=100
DUR=4
PROFILE=""
CLIENT="${YW_CLIENT:-yw-client}"
OUT=""
GAP=1
START=0

usage() {
    # 打印文件开头的注释块（第 2 行起，到第一行不是注释的为止）
    awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
}

while getopts "n:d:p:c:o:g:sh" opt; do
    case "$opt" in
        n) N="$OPTARG" ;;
        d) DUR="$OPTARG" ;;
        p) PROFILE="$OPTARG" ;;
        c) CLIENT="$OPTARG" ;;
        o) OUT="$OPTARG" ;;
        g) GAP="$OPTARG" ;;
        s) START=1 ;;
        h) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done

case "$N" in ''|*[!0-9]*) echo "-n 要正整数：${N}" >&2; exit 2 ;; esac
case "$DUR" in ''|*[!0-9]*) echo "-d 要正整数秒：${DUR}" >&2; exit 2 ;; esac
case "$GAP" in ''|*[!0-9.]*) echo "-g 要秒数：${GAP}" >&2; exit 2 ;; esac
if [ "$N" -lt 1 ] || [ "$DUR" -lt 1 ]; then
    echo "-n、-d 至少为 1" >&2
    exit 2
fi
if ! command -v "$CLIENT" >/dev/null 2>&1; then
    echo "找不到 yw-client：${CLIENT}（用 -c 或环境变量 YW_CLIENT 指定）" >&2
    exit 2
fi

if [ -z "$OUT" ]; then
    OUT="$(mktemp -d "${TMPDIR:-/tmp}/m2-connect.XXXXXX")"
fi
mkdir -p "$OUT"
JSONL="${OUT}/connect.jsonl"
: >"$JSONL"

ARGS=(--headless --connect-once --duration "$DUR" --json)
if [ -n "$PROFILE" ]; then
    ARGS+=(--profile "$PROFILE")
fi
if [ "$START" = 1 ]; then
    ARGS+=(--start)
fi

now_ms() {
    perl -MTime::HiRes=time -e 'printf "%d\n", time() * 1000'
}

STOP=0
trap 'STOP=1' INT TERM

echo "连续连接 ${N} 次，每次 ${DUR} s，结果目录 ${OUT}" >&2
done_runs=0
for i in $(seq 1 "$N"); do
    if [ "$STOP" = 1 ]; then
        echo "被中断，已跑 ${done_runs} 次" >&2
        break
    fi
    out_f="${OUT}/run-${i}.out"
    log_f="${OUT}/run-${i}.log"
    t0="$(now_ms)"
    rc=0
    "$CLIENT" "${ARGS[@]}" >"$out_f" 2>"$log_f" </dev/null || rc=$?
    t1="$(now_ms)"
    wall=$((t1 - t0))
    line="$(tail -n 1 "$out_f" 2>/dev/null || true)"
    case "$line" in
        '{'*'}') rec="{\"i\":${i},\"rc\":${rc},\"wall_ms\":${wall},${line#\{}" ;;
        *) rec="{\"i\":${i},\"rc\":${rc},\"wall_ms\":${wall},\"ok\":false,\"fail_reason\":\"no_json\"}" ;;
    esac
    printf '%s\n' "$rec" >>"$JSONL"
    printf '%s\n' "$rec"
    done_runs=$i
    case "$rec" in
        *'"ok":true'*) ;;
        *) echo "第 ${i} 次失败（rc=${rc}），日志：${log_f}" >&2 ;;
    esac
    if [ "$i" -lt "$N" ] && [ "$STOP" = 0 ]; then
        sleep "$GAP"
    fi
done

# 汇总（BWK awk 也能跑：不用捕获组、不用 asort）
SUMMARY="$(awk '
function num(s, key,    pat) {
    pat = "\"" key "\":-?[0-9.]+"
    if (match(s, pat)) return substr(s, RSTART + length(key) + 3, RLENGTH - length(key) - 3)
    return ""
}
function str(s, key,    pat) {
    pat = "\"" key "\":\"[^\"]*\""
    if (match(s, pat)) return substr(s, RSTART + length(key) + 4, RLENGTH - length(key) - 5)
    return ""
}
function pct(p,    k) {
    k = int(p * nf + 0.999999)
    if (k < 1) k = 1
    if (k > nf) k = nf
    return ff[k]
}
{
    runs++
    if ($0 ~ /"ok":true/) ok++
    else {
        r = str($0, "fail_reason"); if (r == "") r = "unknown"
        fail[r]++
    }
    h = num($0, "hs_attempts")
    if (h != "") {
        nh++; hsum += h; if (h + 0 > hmax) hmax = h + 0
        if (h + 0 <= 1) h1++; else if (h + 0 == 2) h2++; else if (h + 0 == 3) h3++; else h4++
    }
    f = num($0, "first_frame_ms")
    if (f != "") ff[++nf] = f + 0
}
END {
    # 插入排序（次数不多）
    for (i = 2; i <= nf; i++) { v = ff[i]; j = i - 1; while (j >= 1 && ff[j] > v) { ff[j + 1] = ff[j]; j-- } ff[j + 1] = v }
    printf "{\"summary\":true,\"runs\":%d,\"ok\":%d,\"success_rate\":%.4f,", runs, ok, runs ? ok / runs : 0
    printf "\"hs_attempts\":{\"n\":%d,\"1\":%d,\"2\":%d,\"3\":%d,\"4+\":%d,\"max\":%d,\"mean\":%.3f},", nh, h1, h2, h3, h4, hmax, nh ? hsum / nh : 0
    if (nf) printf "\"first_frame_ms\":{\"n\":%d,\"min\":%.1f,\"p50\":%.1f,\"p95\":%.1f,\"max\":%.1f},", nf, ff[1], pct(0.5), pct(0.95), ff[nf]
    else printf "\"first_frame_ms\":{\"n\":0},"
    printf "\"fail_reasons\":{"
    sep = ""
    for (r in fail) { printf "%s\"%s\":%d", sep, r, fail[r]; sep = "," }
    printf "}}\n"
}' "$JSONL")"

printf '%s\n' "$SUMMARY" >"${OUT}/summary.json"
printf '%s\n' "$SUMMARY"

runs="$(wc -l <"$JSONL" | tr -d ' ')"
oks="$(grep -c '"ok":true' "$JSONL" || true)"
echo "汇总：${oks}/${runs} 次成功；明细 ${JSONL}，汇总 ${OUT}/summary.json" >&2
if [ "$runs" -gt 0 ] && [ "$oks" = "$runs" ] && [ "$runs" = "$N" ]; then
    exit 0
fi
exit 1
