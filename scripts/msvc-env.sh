#!/usr/bin/env bash
# 加载 VS2022 BuildTools 的 x64 MSVC 构建环境（PATH / INCLUDE / LIB）。
#
# 为什么需要它：
#   Git Bash 自带 /usr/bin/link.exe（GNU coreutils 的 link），在 PATH 中优先于
#   MSVC 的链接器 link.exe。rustc 链接时调用 `link.exe` 会命中 GNU 版本，报
#   "missing operand" / "extra operand" 而失败。
#   常规解法是 `call vcvars64.bat`，但那需要 cmd.exe（沙箱内被策略拦截）。
#   本脚本直接手工导出三组环境变量，等效于 vcvars64，且无需 cmd.exe。
#
# 用法（在仓库根目录）：
#   source scripts/msvc-env.sh
#   cargo build
#
# 可选覆盖：MSVC_DIR / SDK_DIR / SDK_VER 环境变量。

set -euo pipefail

_msenv_find() {
  local base="$1" pattern="$2"
  local -a hits
  # 用 shell glob（比 find 快且稳定）：base 为 VS 根目录，pattern 为相对 glob。
  shopt -s nullglob
  hits=( "$base"/$pattern )
  shopt -u nullglob
  [ "${#hits[@]}" -gt 0 ] && printf '%s' "${hits[0]}"
}

MSVC_DIR="${MSVC_DIR:-}"
SDK_DIR="${SDK_DIR:-}"
SDK_VER="${SDK_VER:-}"

# --- 定位 MSVC 工具集目录 ---
if [ -z "$MSVC_DIR" ]; then
  for root in \
    "/c/Program Files (x86)/Microsoft Visual Studio" \
    "/c/Program Files/Microsoft Visual Studio" \
    "/d/Program Files/Microsoft Visual Studio" \
    "/d/Program Files (x86)/Microsoft Visual Studio"
  do
    [ -d "$root" ] || continue
    # 兼容两种布局：<root>/<年份>/<产品>/VC/... 与 <root>/BuildTools/VC/...
    hit="$(_msenv_find "$root" "*/*/VC/Tools/MSVC/*/bin/Hostx64/x64/cl.exe")"
    [ -z "$hit" ] && hit="$(_msenv_find "$root" "*/VC/Tools/MSVC/*/bin/Hostx64/x64/cl.exe")"
    if [ -n "$hit" ]; then
      # hit 形如 .../MSVC/<ver>/bin/Hostx64/x64/cl.exe → 回退 4 层到 <ver>
      MSVC_DIR="$(dirname "$(dirname "$(dirname "$(dirname "$hit")")")")"
      break
    fi
  done
fi

# --- 定位 Windows SDK ---
if [ -z "$SDK_DIR" ]; then
  for root in "/d/Windows Kits/10" "/c/Program Files (x86)/Windows Kits/10"; do
    [ -d "$root/Lib" ] && { SDK_DIR="$root"; break; }
  done
fi
if [ -z "$SDK_VER" ] && [ -n "$SDK_DIR" ]; then
  SDK_VER="$(ls -1 "$SDK_DIR/Lib" 2>/dev/null | sort -V | tail -1)"
fi

if [ -z "$MSVC_DIR" ] || [ -z "$SDK_DIR" ] || [ -z "$SDK_VER" ]; then
  echo "[msvc-env] 未能定位 MSVC 工具集或 Windows SDK" >&2
  echo "  MSVC_DIR='${MSVC_DIR:-}' SDK_DIR='${SDK_DIR:-}' SDK_VER='${SDK_VER:-}'" >&2
  echo "  请安装 “使用 C++ 的桌面开发” 工作负载，或显式导出 MSVC_DIR/SDK_DIR/SDK_VER。" >&2
  return 1 2>/dev/null || exit 1
fi

export MSVC_DIR SDK_DIR SDK_VER
_w() { cygpath -w "$1"; }

# MSVC 必须排在 GNU /usr/bin 之前，link.exe 才会解析到 MSVC 版本
export PATH="$MSVC_DIR/bin/Hostx64/x64:$SDK_DIR/bin/$SDK_VER/x64:$PATH"
export INCLUDE="$(_w "$MSVC_DIR/include");$(_w "$SDK_DIR/Include/$SDK_VER/ucrt");$(_w "$SDK_DIR/Include/$SDK_VER/um");$(_w "$SDK_DIR/Include/$SDK_VER/shared")"
export LIB="$(_w "$MSVC_DIR/lib/x64");$(_w "$SDK_DIR/Lib/$SDK_VER/ucrt/x64");$(_w "$SDK_DIR/Lib/$SDK_VER/um/x64")"

echo "[msvc-env] MSVC $(_w "$MSVC_DIR")"
echo "[msvc-env] SDK  $SDK_VER"
echo "[msvc-env] link -> $(command -v link.exe)"
