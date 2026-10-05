#!/bin/bash
# ============================================================
#  跑跑看：把一張圖片變成聲音，再從聲音把圖片救回來
#
#  這個檔案在 Finder 裡雙擊就會打開「終端機」來執行。
#  它會做四件事：
#    1. 用 Rust 編譯程式（第一次大約 20 秒）。
#    2. 示範一次完整流程：圖片 → 聲音檔 → 模擬「喇叭＋房間＋麥克風」
#       → 從錄音解回圖片，並把「聽 15 秒、30 秒、45 秒…」各自的圖片存下來。
#    3. 跑完整的量測並做成一個網頁報告（大約 1～2 分鐘），自動打開。
#    4. 問你要不要做「真的用喇叭播、用麥克風錄」的測試（預設不做）。
#
#  需要先裝好 Rust（提供 cargo）：到 https://rustup.rs 照指示安裝。
# ============================================================

# 先切換到這個檔案所在的資料夾。資料夾名稱有空白和中文，
# 所以 "$(dirname "$0")" 一定要用雙引號包起來。
cd "$(dirname "$0")" || exit 1

pause_and_exit() {
  echo
  read -r -p "按 Enter 關閉視窗..."
  exit "$1"
}

# Homebrew 安裝的 rustup 不一定在 PATH 裡，先幫忙加上。
if ! command -v cargo >/dev/null 2>&1; then
  for d in "$HOME/.cargo/bin" /opt/homebrew/opt/rustup/bin; do
    if [ -x "$d/cargo" ]; then
      export PATH="$d:$PATH"
      break
    fi
  done
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "找不到 cargo（Rust 的編譯工具）。"
  echo "請打開 https://rustup.rs ，照網頁上的一行指令安裝後再雙擊一次。"
  pause_and_exit 1
fi

echo "== 1/4 編譯程式（Rust，release 模式）..."
if ! cargo build --release -j 4; then
  echo "編譯失敗，上面的訊息會說明原因。"
  pause_and_exit 1
fi
BIN=./target/release/chirpix

echo
echo "== 2/4 示範：圖片 → 聲音 → 模擬的房間 → 圖片"
mkdir -p out/demo
if [ -f data/kodim23.png ]; then
  PIC=data/kodim23.png
  echo "使用 data/ 裡的柯達測試照片 kodim23.png。"
else
  PIC=out/demo/scene.png
  "$BIN" testimage scene -o "$PIC" --size 384x256 >/dev/null
  echo "使用程式自己畫的測試圖（out/demo/scene.png）。"
  echo "想改用真的照片：在這個資料夾執行 make data（會下載 4 張柯達公開測試照片，共 2.8 MB）。"
fi
echo
echo "-- 把圖片編成 90 秒的聲音檔 out/demo/tx.wav"
"$BIN" encode "$PIC" -o out/demo/tx.wav || pause_and_exit 1
echo
echo "-- 讓聲音通過模擬的「普通房間」（有回音、雜訊，錄音的人晚了 7.3 秒才按下錄音）"
"$BIN" simulate out/demo/tx.wav -o out/demo/rx.wav --channel fair --skip 7.3 || pause_and_exit 1
echo
echo "-- 從錄音解回圖片（每 15 秒存一張，看得出越聽越清楚）"
"$BIN" decode out/demo/rx.wav -o out/demo/decoded --every 15 --ref "$PIC" || pause_and_exit 1
echo
echo "各個時間點的圖片在 out/demo/decoded/ 資料夾裡（t0015.png、t0030.png…final.png）。"

echo
echo "== 3/4 完整量測並產生報告（大約 1～2 分鐘，請稍等）"
if [ -f data/kodim23.png ]; then
  "$BIN" report -o out/report --threads 4 data/kodim23.png data/kodim19.png data/kodim05.png data/kodim08.png 2>/dev/null
else
  "$BIN" report -o out/report --threads 4 2>/dev/null
fi
if [ -f out/report/report.html ]; then
  echo "報告在 out/report/report.html ，現在幫你打開。"
  open out/report/report.html
  open out/demo/decoded
else
  echo "報告沒有產生，請看上面的錯誤訊息。"
  pause_and_exit 1
fi

echo
echo "== 4/4（選做）真的用喇叭播、用麥克風錄"
echo "上面的結果全部是「模擬的房間」。這一步會用這台電腦的喇叭把聲音播出來（大約 50 秒，"
echo "聽起來像沙沙的雜音加上咻咻聲），同時用麥克風錄下來，再解回圖片。"
echo "注意："
echo "  - 音量請先調到中等，太小聲會收不到，太大聲喇叭會破音。"
echo "  - 第一次執行時 macOS 會跳出視窗問「終端機想要取用麥克風」，要按「好」才錄得到。"
echo "  - 這一步作者沒有辦法事先在真的喇叭上測過，結果好壞都請當成實驗。"
read -r -p "要做嗎？輸入 y 再按 Enter 開始；直接按 Enter 就跳過： " answer
case "$answer" in
  y|Y)
    if ! command -v swift >/dev/null 2>&1; then
      echo "找不到 swift（Xcode 的命令列工具）。請先在終端機執行 xcode-select --install 再試一次，"
      echo "或改用下面的「兩台裝置」做法。"
    else
      mkdir -p out/air
      "$BIN" encode "$PIC" -o out/air/tx.wav --design 40 --seconds 48 >/dev/null
      echo "開始播放並錄音，大約 50 秒，期間請保持安靜、不要移動電腦..."
      if swift scripts/audio_loop.swift --play out/air/tx.wav --record out/air/rx.wav; then
        echo
        if "$BIN" decode out/air/rx.wav -o out/air/decoded --every 8 --ref "$PIC"; then
          echo
          echo "成功。圖片在 out/air/decoded/ ，幫你打開最後一張。"
          open out/air/decoded/final.png
        else
          echo
          echo "這次沒有解出圖片。常見原因：音量太小或太大、環境太吵、麥克風權限沒有給。"
          echo "錄音檔留在 out/air/rx.wav ，可以用播放器聽聽看有沒有錄到沙沙聲。"
        fi
      else
        echo "播放或錄音沒有成功（上面一行是原因）。"
      fi
    fi
    ;;
  *)
    echo "跳過。"
    ;;
esac

echo
echo "另一種玩法（兩台裝置）："
echo "  1. 用這台電腦播放 out/demo/tx.wav（在 Finder 裡按空白鍵就能播）。"
echo "  2. 用手機任何一個錄音 App 錄 30 秒以上（從中間開始錄也可以），把錄音檔傳回電腦。"
echo "  3. 如果不是 .wav 檔，先轉檔：afconvert -f WAVE -d LEI16 錄音.m4a 錄音.wav"
echo "  4. 解碼：./target/release/chirpix decode 錄音.wav -o out/phone"
pause_and_exit 0
