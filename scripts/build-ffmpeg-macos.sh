#!/bin/bash
# assets/bin/ffmpeg / ffprobe を macOS arm64 向けに自前ビルドする。
#
# VJDownloader が使う機能（h264_videotoolbox エンコード、ネイティブデコーダ、
# audiotoolbox 出力、rawvideo パイプ、https 入力、ffprobe のタグ取得）は
# すべて FFmpeg 内蔵で足りるため、外部ライブラリを一切リンクしない。
# その結果ライセンスは LGPL-2.1-or-later となり、GPL 版より大幅に小さくなる。
#
# 使い方:
#   scripts/build-ffmpeg-macos.sh            # ビルドと検証のみ
#   scripts/build-ffmpeg-macos.sh --install  # 検証後 assets/bin/ へ配置
#
# 必要なもの: Xcode Command Line Tools のみ（Homebrew も nasm も不要）

set -euo pipefail

# アプリバンドルの LSMinimumSystemVersion と揃える。ずれると古い macOS で起動しない。
readonly MIN_MACOS="13.0"
readonly REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly WORK_DIR="${TMPDIR:-/tmp}/vjdl-ffmpeg-build"
readonly SRC_DIR="${WORK_DIR}/FFmpeg"

INSTALL=0
[ "${1:-}" = "--install" ] && INSTALL=1

echo "==> ソース取得: ${SRC_DIR}"
mkdir -p "${WORK_DIR}"
if [ -d "${SRC_DIR}/.git" ]; then
  git -C "${SRC_DIR}" fetch --depth 1 origin master
  git -C "${SRC_DIR}" reset --hard FETCH_HEAD
else
  git clone --depth 1 https://github.com/FFmpeg/FFmpeg.git "${SRC_DIR}"
fi
FFMPEG_COMMIT="$(git -C "${SRC_DIR}" rev-parse --short HEAD)"
echo "    FFmpeg commit: ${FFMPEG_COMMIT}"

cd "${SRC_DIR}"

echo "==> configure"
# --extra-libs=-liconv:
#   --disable-autodetect では iconv の検出が走らずリンクフラグが付かないため必須。
#   これが無いと _iconv_open 未解決でリンクに失敗する。
# -mmacosx-version-min:
#   MACOSX_DEPLOYMENT_TARGET 環境変数は make に伝わらないため configure フラグで渡す。
#   これが無いとビルドホストの OS バージョンが最小要件になってしまう。
./configure \
  --disable-autodetect \
  --disable-debug \
  --disable-doc \
  --disable-ffplay \
  --enable-videotoolbox \
  --enable-audiotoolbox \
  --enable-securetransport \
  --enable-zlib \
  --enable-bzlib \
  --enable-iconv \
  --extra-cflags="-mmacosx-version-min=${MIN_MACOS}" \
  --extra-ldflags="-mmacosx-version-min=${MIN_MACOS}" \
  --extra-libs=-liconv

# configure のフラグを変えたあとにオブジェクトが残っていると
# デプロイメントターゲットが混在するため、必ず clean から作り直す。
echo "==> make clean && make"
make clean > /dev/null 2>&1 || true
make -j"$(sysctl -n hw.ncpu)" ffmpeg ffprobe

echo "==> 検証"
fail=0
check() {
  if [ "$1" = 0 ]; then echo "    PASS  $2"; else echo "    FAIL  $2"; fail=1; fi
}

for bin in ffmpeg ffprobe; do
  minos="$(otool -l "${SRC_DIR}/${bin}" | awk '/LC_BUILD_VERSION/,/sdk/ {if ($1 == "minos") {print $2; exit}}')"
  [ "${minos}" = "${MIN_MACOS}" ]
  check $? "${bin} minos = ${MIN_MACOS} (実際: ${minos})"

  # システム以外の dylib に依存していると配布先で動かない
  extra="$(otool -L "${SRC_DIR}/${bin}" | tail -n +2 | grep -vE '/System/|/usr/lib/' || true)"
  [ -z "${extra}" ]
  check $? "${bin} 非システム依存なし"
done

# GPL コンポーネントが混入していないこと（配布物のライセンス条件が変わる）
! "${SRC_DIR}/ffmpeg" -hide_banner -version | grep -q -- "--enable-gpl"
check $? "GPL 無効 (LGPL-2.1-or-later)"

# VJDownloader が実際に投げる引数で通ることを確認する
tmp_mp4="${WORK_DIR}/verify.mp4"
"${SRC_DIR}/ffmpeg" -hide_banner -loglevel error \
  -f lavfi -i "testsrc=size=640x360:rate=30:duration=2" \
  -f lavfi -i "sine=frequency=440:duration=2" \
  -c:v h264_videotoolbox -allow_sw 1 -b:v 5M -pix_fmt yuv420p \
  -c:a aac -b:a 192k -movflags +faststart -metadata comment="テストコメント" \
  -y "${tmp_mp4}" 2>/dev/null
check $? "h264_videotoolbox で MP4 エンコード"

[ "$("${SRC_DIR}/ffprobe" -v error -show_entries format_tags=comment -of default=nw=1:nk=1 "${tmp_mp4}")" = "テストコメント" ]
check $? "ffprobe でコメントタグ取得"

# ストリーム再生のプレビュー経路
bytes="$("${SRC_DIR}/ffmpeg" -hide_banner -loglevel error -i "${tmp_mp4}" \
  -map 0:v -vf "fps=30,scale=480:270" -pix_fmt rgba -f rawvideo pipe:1 2>/dev/null | wc -c | tr -d ' ')"
[ "${bytes}" = "31104000" ]
check $? "rawvideo rgba パイプ出力 (${bytes} bytes)"

"${SRC_DIR}/ffmpeg" -hide_banner -devices 2>&1 | grep -q "E audiotoolbox"
check $? "audiotoolbox 出力デバイス"

# 直リンク再生は https URL を ffmpeg へ直接渡すため TLS バックエンドが必須。
# TLS が無い場合は "Protocol not found"、通っている場合は本文を受け取って
# "Invalid data found"（HTML なので当然）になる。
tls_out="$("${SRC_DIR}/ffmpeg" -hide_banner -loglevel error -i https://www.google.com/ -f null - 2>&1 || true)"
echo "${tls_out}" | grep -q "Invalid data found"
check $? "https/tls 接続"

rm -f "${tmp_mp4}"

if [ "${fail}" != 0 ]; then
  echo "==> 検証に失敗したため中断します" >&2
  exit 1
fi

echo
echo "==> 完了 (FFmpeg ${FFMPEG_COMMIT})"
ls -l "${SRC_DIR}/ffmpeg" "${SRC_DIR}/ffprobe" | awk '{printf "    %-40s %6.1f MB\n", $9, $5/1048576}'

if [ "${INSTALL}" = 1 ]; then
  echo "==> assets/bin/ へ配置"
  cp "${SRC_DIR}/ffmpeg" "${REPO_ROOT}/assets/bin/ffmpeg"
  cp "${SRC_DIR}/ffprobe" "${REPO_ROOT}/assets/bin/ffprobe"
  # git の記録モード (100644) と揃える。実行権限はアプリ側が配置時に付与する。
  chmod 644 "${REPO_ROOT}/assets/bin/ffmpeg" "${REPO_ROOT}/assets/bin/ffprobe"
  echo "    配置しました。THIRD_PARTY_NOTICES.md のコミットハッシュも更新してください。"
else
  echo "    assets/bin/ へ配置するには --install を付けて実行してください。"
fi
