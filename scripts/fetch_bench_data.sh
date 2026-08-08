#!/usr/bin/env bash
# ==============================================================================
# fetch_bench_data.sh — 从 Wikipedia 抓取 HTML 解析器 Benchmark 测试数据
# ==============================================================================
#
# 数据来源: Wikimedia Foundation (CC BY-SA 4.0)
#   https://creativecommons.org/licenses/by-sa/4.0/
#
# 生成文件:
#   benches/data/lipsum_10kb.html        — "Parsing" 词条,  ≥  10 KB
#   benches/data/wikipedia_fragment.html — "HTML" 词条,     ≥ 100 KB
#   benches/data/large_doc_2mb.html      — "Web browser" 词条, ≥ 2 MB
#
# 每个输出文件的 <head> 中注入来源 URL 与抓取时间戳，满足学术可复现性要求。
#
# 用法:
#   cd crates/muskitty-html5-parser
#   bash scripts/fetch_bench_data.sh
# ==============================================================================

set -euo pipefail

# ── Paths ────────────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA_DIR="$PROJECT_DIR/benches/data"

# ── Colours (ANSI) ───────────────────────────────────────────────────────────
BOLD='\033[1m'
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

ok()   { printf "${GREEN}✅${NC} %s\n" "$*"; }
fail() { printf "${RED}❌${NC} %s\n" "$*" >&2; }
info() { printf "${CYAN}📥${NC} %s\n" "$*"; }
warn() { printf "${YELLOW}⚠️${NC}  %s\n" "$*"; }
step() { printf "\n${BOLD}── %s ──${NC}\n" "$*"; }

# ── Portable human-readable size ─────────────────────────────────────────────
human_size() {
    local bytes=$1
    if [ "$bytes" -ge 1048576 ]; then
        awk "BEGIN { printf \"%.1f MB\", $bytes / 1048576 }"
    elif [ "$bytes" -ge 1024 ]; then
        awk "BEGIN { printf \"%.0f KB\", $bytes / 1024 }"
    else
        echo "${bytes} B"
    fi
}

# ── Cleanup trap ─────────────────────────────────────────────────────────────
cleanup() {
    rm -f "$TMP_RAW" "$TMP_ATTR" "$TMP_FINAL"
}
trap cleanup EXIT

# ── Dependency check ─────────────────────────────────────────────────────────
echo -e "${BOLD}🔍 检查依赖...${NC}"

if ! command -v curl &>/dev/null; then
    fail "curl 未安装。请先安装 curl:  brew install curl  或  apt install curl"
    exit 1
fi
ok "curl 已安装。"

# ── Prepare output directory ─────────────────────────────────────────────────
mkdir -p "$DATA_DIR"

placeholder_count=$(find "$DATA_DIR" -maxdepth 1 -name '*.placeholder' 2>/dev/null | wc -l | tr -d ' ')
if [ "$placeholder_count" -gt 0 ]; then
    rm -f "$DATA_DIR"/*.placeholder
    info "已清理 ${placeholder_count} 个 placeholder 文件。"
fi

# =============================================================================
# fetch_article <title> <output_filename> <min_size_kb>
# =============================================================================
fetch_article() {
    local title="$1"
    local output_name="$2"
    local min_kb="$3"
    local min_bytes=$((min_kb * 1024))
    local output_path="$DATA_DIR/$output_name"
    local url_title="${title// /_}"
    local url="https://en.wikipedia.org/w/index.php?title=${url_title}&printable=yes"
    local timestamp
    timestamp=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

    step "Wikipedia: ${title}  (目标 ≥ ${min_kb} KB)"

    # ── Backup existing file ──────────────────────────────────────────────
    if [ -f "$output_path" ]; then
        cp "$output_path" "${output_path}.bak"
        info "已备份旧文件 → ${output_name}.bak"
    fi

    # ── Temporary files ───────────────────────────────────────────────────
    TMP_RAW=$(mktemp)
    TMP_ATTR=$(mktemp)
    TMP_FINAL=$(mktemp)

    # ── Fetch with retry ──────────────────────────────────────────────────
    local retry=0 max_retry=3 fetch_ok=false

    while [ $retry -lt $max_retry ]; do
        # -L          follow redirects
        # -f          fail on HTTP errors (4xx/5xx)
        # -s          silent (no progress bar)
        # -S          show errors even in silent mode
        # --max-time  120 s timeout for large pages
        if curl -L -f -s -S \
            --connect-timeout 15 \
            --max-time 120 \
            -A "muskitty-bench-fetcher/1.0 (academic research; CC-BY-SA 4.0)" \
            -o "$TMP_RAW" \
            "$url"; then
            fetch_ok=true
            break
        fi
        retry=$((retry + 1))
        warn "抓取失败，重试 (${retry}/${max_retry})..."
        sleep 2
    done

    if ! $fetch_ok; then
        fail "无法抓取 ${title} (已重试 ${max_retry} 次)。"
        return 1
    fi

    # ── Inject attribution comment at top of file ─────────────────────────
    local attr_comment
    attr_comment="<!-- Source: ${url} | Timestamp: ${timestamp} | License: CC BY-SA 4.0 (Wikimedia Foundation) -->"
    {
        echo "$attr_comment"
        cat "$TMP_RAW"
    } > "$TMP_ATTR"

    local single_bytes
    single_bytes=$(wc -c < "$TMP_ATTR" | tr -d ' ')
    info "单次抓取: $(human_size $single_bytes)"

    # ── Build final output (repeat content if needed) ─────────────────────
    if [ "$single_bytes" -ge "$min_bytes" ]; then
        # Single fetch is enough
        cp "$TMP_ATTR" "$TMP_FINAL"
    else
        # Need to repeat content to reach target size
        local repeats=$(( (min_bytes + single_bytes - 1) / single_bytes ))
        info "需要拼接 ${repeats} 份 (共约 $(human_size $((single_bytes * repeats))))"

        # Strategy: extract the <head> section once (with attribution),
        # then repeat <body> content N times inside a single valid document.
        {
            # Everything up to and including <body> tag (minus the <body> tag itself)
            sed -n '1,/<body[^>]*>/p' "$TMP_ATTR" | sed '$s/<body[^>]*>//'

            # Body content from the original fetch (without <body> / </body> wrappers)
            local body_content
            body_content="$(sed -n '/<body[^>]*>/,/<\/body>/p' "$TMP_RAW" | sed '1s/<body[^>]*>//; $s/<\/body>//')"

            echo "<body>"

            local i
            for i in $(seq 1 "$repeats"); do
                printf "<!-- repeat %d / %d -->\n" "$i" "$repeats"
                printf '%s\n' "$body_content"
            done

            echo "</body>"
            echo "</html>"
        } > "$TMP_FINAL"
    fi

    mv "$TMP_FINAL" "$output_path"

    # ── Verify ────────────────────────────────────────────────────────────
    local final_bytes
    final_bytes=$(wc -c < "$output_path" | tr -d ' ')
    local final_human
    final_human=$(human_size "$final_bytes")

    if [ "$final_bytes" -ge "$min_bytes" ]; then
        ok "${output_name} 生成完毕 (${final_human})。"
    else
        fail "${output_name} 大小不足: ${final_human} (需要 ≥ ${min_kb} KB)。"
        return 1
    fi
}

# =============================================================================
# Main
# =============================================================================
echo ""
echo -e "${BOLD}════════════════════════════════════════════════════${NC}"
echo -e "${BOLD}  MusKitty Benchmark 数据抓取工具${NC}"
echo -e "${BOLD}  数据来源: Wikimedia Foundation (CC BY-SA 4.0)${NC}"
echo -e "${BOLD}════════════════════════════════════════════════════${NC}"

fetch_article "Parsing"       "lipsum_10kb.html"        10
fetch_article "HTML"          "wikipedia_fragment.html" 100
fetch_article "Web browser"   "large_doc_2mb.html"      2048

echo ""
echo -e "${BOLD}${GREEN}🎉 Benchmark 数据准备就绪！${NC}"
echo ""
echo -e "   数据目录: ${CYAN}${DATA_DIR}${NC}"
echo ""
echo -e "   ${BOLD}文件清单:${NC}"
printf "   %-35s %s\n" "lipsum_10kb.html"        "$(human_size $(wc -c < "$DATA_DIR/lipsum_10kb.html" | tr -d ' '))"
printf "   %-35s %s\n" "wikipedia_fragment.html" "$(human_size $(wc -c < "$DATA_DIR/wikipedia_fragment.html" | tr -d ' '))"
printf "   %-35s %s\n" "large_doc_2mb.html"      "$(human_size $(wc -c < "$DATA_DIR/large_doc_2mb.html" | tr -d ' '))"
echo ""
echo -e "   ${BOLD}下一步:${NC}"
echo -e "   ${CYAN}cargo bench --bench parser_bench${NC}"
echo ""
echo -e "   ${YELLOW}⚠ 学术发表时请在论文中注明数据来源:${NC}"
echo -e "   ${YELLOW}  Wikimedia Foundation. Content available under CC BY-SA 4.0.${NC}"
echo -e "   ${YELLOW}  Fetched on $(date -u +"%Y-%m-%d").${NC}"
