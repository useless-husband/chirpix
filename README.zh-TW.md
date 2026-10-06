# chirpix（聲音傳圖片）

把一張圖片用聲音從空氣中傳過去。一台裝置用喇叭播放 WAV 檔，另一台用麥克風錄音，錄音可以解回圖片。
數據機、錯誤更正碼、影像編碼、WAV／PNG 檔案讀寫全部都在這個專案裡，用 Rust 寫成，除了標準函式庫沒有任何相依套件。

用聲音傳資料是很老的技術，現成的工具很多（見[相關作品](#相關作品)）；**這是一個學習性質的重做，不是新點子**。
它模仿的是柏克萊大學 EE123 課程的期末專題（75 秒內透過音訊通道傳出最好的圖片）。這個專案想做好的是把三層設計接在一起，讓

- **圖片越聽越清楚**，而不是整個檔案收完才出現；
- **什麼時候開始聽都可以**，中間漏掉幾段也沒關係；

然後老實量出這樣做的代價。

> **證據的範圍。** 下面所有數字都來自「模擬的」喇叭—房間—麥克風通道。訊號另外有透過 macOS 真正的音效系統、
> 經由虛擬迴路裝置播放並錄回來（沒有經過空氣）。**還沒有**用真的喇叭和麥克風測過。
> 類比 SSTV 對照組以及所有跟它有關的數字，是在一台四個 vCPU 的雲端 Linux 虛擬機上跑的；chirpix 的數字在那台機器上
> 重跑，結果和在 Apple M5 上量的完全相同。

[English](README.md) · [設計筆記（英文）](docs/DESIGN.md) · [給初學者的導讀](docs/導讀.zh-TW.md)

![報告：聽 5、15、30、75 秒後的圖片](docs/report.png)

## 結果摘要

四張柯達測試照片（768x512），一個普通的模擬房間（「fair」：頻帶內訊雜比 14 dB、殘響時間 RT60 0.45 秒、
直達聲比回音大 5 dB、錄音端時鐘慢 70 ppm、偶爾有爆音），聆聽者在四個任意時刻開始錄。
數字是 RGB 的平均 PSNR／亮度的 SSIM；括號內是 16 次測試中有幾次有圖。沒有圖的那次以「全灰畫面」計分（12.4 dB／0.41）。
前四列用同一個數據機和影像編碼，只有封包排程不同；後三列是類比 SSTV（Robot 36），經過同一個通道、同樣的亂數種子和開始時間。
36.9 秒是一張 SSTV 圖（含標頭）的長度。

| 做法 | 5 秒 | 15 秒 | 30 秒 | 36.9 秒 | 75 秒 |
|---|---|---|---|---|---|
| **chirpix**（分層噴泉碼，資料流 9.1 kB） | 19.1 dB／0.48（16） | 20.5／0.51（16） | 22.3／0.57（16） | 22.8／0.60（16） | 26.4／0.74（16） |
| 漸進式資料流照順序送、循環播放 | 12.4／0.41（0） | 14.6／0.44（4） | 15.7／0.48（4） | 18.2／0.53（8） | 31.1／0.87（16） |
| 一般噴泉碼，收完才顯示 | （0） | （0） | （0） | （0） | 30.9／0.87（16） |
| 普通 27 kB 檔案，75 秒送一遍 | （0） | （0） | （0） | （0） | **31.1／0.87**（16） |
| SSTV Robot 36（320x240），聽到標頭之後才顯示 | （0） | 12.4／0.41（4） | 12.9／0.40（8） | 13.3／0.40（16） | 16.9／0.42（16） |
| SSTV，標頭之前聽到的掃描線也保留 | （0） | 12.8／0.41（4） | 13.9／0.41（8） | 16.9／0.41（16） | 16.9／0.42（16） |
| SSTV，聆聽者剛好在一張圖開始時就在聽 | 12.6／0.40（16） | 13.2／0.39（16） | 15.1／0.41（16） | 16.9／0.41（16） | 16.9／0.41（16） |

兩個方向都要看。一分鐘以內，只有 chirpix 穩定地有圖。到了 75 秒、通道又完全不掉封包時，
**普通檔案贏 4.7 dB**：它把所有時間都拿來送新資料，而 chirpix 有三分之二的時間花在「讓前面幾層早點到」
（背後的數學上限寫在 [DESIGN.md](docs/DESIGN.md)）。

在「poor」通道（訊雜比 10 dB、RT60 0.6 秒、回音和直達聲一樣大、爆音、錄音會掉音、18% 封包遺失），
每個時間點的順序都反過來：chirpix 是 17.4／20.0／22.2／24.8 dB，16 次中分別有 13、16、16、16 次有圖；
普通檔案 75 秒內一次都沒收完；照順序送的做法到 20.7 dB；SSTV 在 75 秒是 14.2 dB（16 次都有圖）。

**和類比 SSTV（業餘無線電傳圖的方式）比**，在三個模擬房間、每個時間點都是 chirpix 贏。聽完一整張 SSTV 圖時（36.9 秒），
「fair」通道上 SSTV 是 16.9 dB、chirpix 是 22.8 dB；到 75 秒 SSTV 還是 16.9、chirpix 26.4。主因是房間回音：
FM 鑑頻器對「強度有直達聲三分之一」的回音毫無抵抗力（OFDM 數據機那 43 毫秒的循環字首就是為了這個）。其次是解析度：
320x240 讓四張照片平均最多只有 25.7 dB，完全不經過通道、送出再收回來也只有 22.2 dB。SSTV 贏的地方：

- **它沒有門檻。** 只有白雜訊、沒有回音時，數據機在 4 到 2 dB 之間收不到封包，chirpix 什麼都顯示不出來；SSTV 在 2 dB
  還有 15.7 dB、0 dB 還有 14.2 dB。（SSIM 認為這些滿是雜訊的圖比全灰畫面還差，PSNR 則認為比較好。）
- **沒有起始延遲**，前提是聆聽者在一張圖開始時就在聽：大約一秒後第一條掃描線就出現。但 5 秒時只有最上面 27 條（12.6 dB），
  chirpix 則已經有整張粗略的圖（19.1 dB）。如果像表中那樣從一張圖的中間開始聽，就要等下一個標頭，最多 38 秒；
  除非接收端把標頭之前聽到的掃描線也留下來（第二列 SSTV）。
- 它是漸漸變差：從「good」到「poor」，75 秒時從 19.7 dB 滑到 14.2 dB；普通檔案和一般噴泉碼則是從 35 dB 直接變成什麼都沒有。

SSTV 編碼器和 pySSTV 逐個取樣點相符，解碼器也拿 pySSTV 產生的聲音驗證過；細節見 [DESIGN.md](docs/DESIGN.md#analogue-baseline-sstv-robot-36)。

![PSNR 對聆聽時間、對開始時間](docs/curves.png)

下面那張圖是第二個特性：聽 30 秒之後，不管從哪一秒開始，chirpix 都是 24.8 到 27.6 dB；
照順序送的做法只有在這 30 秒剛好包含循環的開頭時才有圖（那時候它比較好，最高 34 dB）。

以上全部可以用 `make data && make report` 重現，會產生單一檔案的 `out/report/report.html`：加入 SSTV 之前，
Apple M5 四核心約 90 秒（量測時機器同時有其他工作在跑）；加入之後，在四個 vCPU 的雲端 Linux 虛擬機上是 5 分 8 秒
（同一台機器不含 SSTV 是 3 分 52 秒）。

## 試試看

```
make build          # 需要 Rust 1.82 以上（https://rustup.rs）
make test           # 84 個測試，編譯完之後幾秒鐘
make sstv-check     # SSTV 對照 pySSTV 和 sstv 套件（需要 python3 和連上 PyPI）
make demo           # 用內建測試圖做小報告 -> out/demo/report.html
make data report    # 下載四張柯達照片（2.8 MB，有 SHA-256 檢查）並做完整報告
```

在 macOS 上，直接雙擊 `跑跑看.command`，會一步一步帶你跑（中文說明）。

手動跑一張圖（真實輸出；`simulate` 代替房間）：

```
$ chirpix encode data/kodim23.png -o tx.wav
image        data/kodim23.png  768x512
mode         QPSK rate 1/2, 372 bytes/s of payload
stream       9095 bytes in 85 packets (Windowed scheme, sized for 75 s of listening)
layers       0.4 kB, 1.0 kB, 2.0 kB, 3.3 kB, 5.1 kB, 9.1 kB (each decodable on its own, coarsest first)
audio        tx.wav : 92.1 s, 53 frames of 1.728 s, 48 kHz 16-bit mono

$ chirpix simulate tx.wav -o rx.wav --channel fair --skip 7.3 --rate 44100
channel      SNR 14 dB, RT60 0.45 s, DRR 5 dB, clock -70 ppm, band 0.4-9.0 kHz, 0.5 clicks/s at +20 dB
recording    rx.wav : 84.8 s at 44100 Hz (started 7.3 s into the transmission)

$ chirpix decode rx.wav -o decoded --every 15 --ref data/kodim23.png
frames       48 found, first at 1.59 s
signal       QPSK, 85 source packets, scheme Windowed
quality      10.1 dB per carrier, recorder clock -72 ppm, packets ok 288/288

  seconds  packets  stream bytes  picture
     15.0       46           963  decoded/t0015.png  PSNR 24.81 dB  SSIM 0.8020
     30.0       97          2033  decoded/t0030.png  PSNR 27.29 dB  SSIM 0.8339
     45.0      150          3317  decoded/t0045.png  PSNR 29.05 dB  SSIM 0.8627
     60.0      202          5136  decoded/t0060.png  PSNR 30.79 dB  SSIM 0.8788
     75.0      253          9095  decoded/t0075.png  PSNR 33.57 dB  SSIM 0.9059  [complete]
```

同一張圖改用類比 SSTV、經過同一個房間（真實輸出；WAV 開頭有 0.25 秒靜音）：

```
$ chirpix sstv-encode data/kodim23.png -o sstv.wav --repeat 3
image        data/kodim23.png  768x512 -> 320x240
mode         Robot 36 (VIS code 8), 150 ms per line, 36.91 s per picture with header, colour Spec
audio        sstv.wav : 111.2 s, 3 picture(s), 48 kHz 16-bit mono

$ chirpix simulate sstv.wav -o rx.wav --channel fair --skip 7.3 --rate 44100
channel      SNR 14 dB, RT60 0.45 s, DRR 5 dB, clock -70 ppm, band 0.4-9.0 kHz, 0.5 clicks/s at +20 dB
recording    rx.wav : 103.9 s at 44100 Hz (started 7.3 s into the transmission)

$ chirpix sstv-decode rx.wav -o decoded-sstv --every 15 --ref data/kodim23.png
recording    rx.wav: 103.9 s at 44100 Hz, 1 channel(s)
header       at 29.86 s: VIS code 8
header       at 66.77 s: VIS code 8
lines        677 sync pulses, 679 lines placed (199 counted back from a header), clock -68 ppm
noise        240 Hz rms on the sync pulses; pixels measured over 15.0 widths (Y), 59.9 (chroma)

  seconds  rows  picture
     15.0     0  nothing yet
     30.0     0  nothing yet
     45.0    94  decoded-sstv/t0045.png  PSNR 13.53 dB  SSIM 0.6519
     60.0   194  decoded-sstv/t0060.png  PSNR 15.86 dB  SSIM 0.6256
     75.0   240  decoded-sstv/t0075.png  PSNR 17.94 dB  SSIM 0.6286  [all rows]
     90.0   240  decoded-sstv/t0090.png  PSNR 17.89 dB  SSIM 0.6293
    103.9   240  decoded-sstv/t0104.png  PSNR 17.86 dB  SSIM 0.6282
```

加上 `--placement buffered`，第一個標頭之前聽到的 199 條掃描線會在標頭到達時放進圖裡：45 秒時 240 列都有了（17.90 dB）。

### 真的從空氣中傳（作者尚未驗證）

兩台裝置：一台播放 `tx.wav`（任何播放器都可以），另一台用手機的錄音 App 錄 30 秒以上，什麼時候開始錄都可以；
把錄音檔傳回電腦，執行 `chirpix decode 錄音.wav`。如果錄音不是 WAV 檔，先轉檔
（macOS：`afconvert -f WAVE -d LEI16 in.m4a out.wav`）。兩台裝置都不要動：每個符號長 0.17 秒，中途移動會有影響。

一台 Mac：`swift scripts/audio_loop.swift --play tx.wav --record rx.wav` 會用目前的輸出裝置播放、
用目前的輸入裝置錄音（第一次 macOS 會詢問麥克風權限），然後 `chirpix decode rx.wav`。
`跑跑看.command` 的最後一步（選做）就是這個。

已經驗證過的是：同一個小工具，播放和錄音兩端都指定 BlackHole 虛擬裝置（`make loopback`）。
36.8 秒的聲音經過 CoreAudio、以裝置的 44.1 kHz 錄回來，QPSK 126/126 包、16-QAM 252/252 包全部正確，
圖片在設定的 30 秒收完。這測到了播放、錄音、取樣率轉換和「沒有對時的開始」；裡面沒有喇叭、房間和麥克風。

## 運作方式

```
圖片 ── 影像編碼 ──► 可隨處截斷的位元組串 ── 噴泉碼 ──► 無窮無盡的封包序列 ── 數據機 ──► 聲音
        小波、位元平面    任何前綴都能解          六個視窗、XOR     第 n 包的內容只由 n 決定     OFDM，每段 1.7 秒
```

- **數據機。** 48 kHz 的 OFDM，1.03–7.03 kHz 之間 1024 個載波，間隔 5.86 Hz，符號長 171 毫秒、循環字首 43 毫秒。
  每一段（1.728 秒）自成一體：一個啁啾聲用來偵測和對時、一個訓練符號、一個重複很多次的標頭、六個資料符號，
  每個資料符號是一個編碼區塊（K=7、碼率 1/2 的迴旋碼，軟判決 Viterbi，CRC-32）。每 8 個載波一個導頻，
  用來量每一段的時鐘誤差；接著整段錄音依量到的誤差重新取樣、再解一次。QPSK 每秒 372 位元組，16-QAM 每秒 743 位元組。
- **噴泉碼。** 第 `n` 包是由 `n` 決定的幾個原始封包的 XOR。資料流開頭有六個由小到大的視窗，各分到固定比例的封包，
  所以開頭最先解出來。接收端用高斯消去法解方程式，使用「從頭開始連續已知」的最長一段。
- **影像編碼。** YCbCr、9/7 小波、位元平面用四元樹加自適應二元區間編碼。資料流是嵌入式的：切在哪裡都能解。
- **類比對照組**（`src/sstv.rs`）。Robot 36 SSTV：照公開的時序編碼；解碼端有 FM 鑑頻、VIS 標頭、
  掃描同步（含時鐘誤差擬合）、用奇偶分隔音在掉音後把行數算對、依量到的雜訊決定平滑程度。

封包格式、設計理由、走過的冤枉路寫在 [docs/DESIGN.md](docs/DESIGN.md)。

## 量測

以下數字來自 `make data report` 產生的 `out/report/report.html`。每個數字是一次模擬或固定亂數種子下幾次的平均，不是信賴區間。

**對照理論（只有白雜訊）。** Es/N0 是每個載波的訊雜比。

| | 未編碼錯誤率 1e-2：教科書 | 數據機 | 編碼後錯誤率 1e-4：理想接收機 | 數據機 | 實作損失 | 封包遺失低於 1% 的起點 |
|---|---|---|---|---|---|---|
| QPSK | 7.2 dB | 8.6 dB | 3.4 dB | 4.1 dB | 0.7 dB | 4.5 dB |
| 16-QAM | 13.9 dB | 15.4 dB | 8.5 dB | 10.0 dB | 1.5 dB | 10.5 dB |

「理想接收機」是同樣的對映、軟位元規則和 Viterbi 解碼器，但時間和通道完全已知；它和這個碼的公開聯集上界吻合。
循環字首、導頻和段落開銷另外還要多花 2.9 dB 的發射能量，不含在上面的數字裡。

**哪裡會壞**（送達的封包比例，QPSK／16-QAM，一次只變一種干擾）：

| 干擾 | 正常的範圍 | 變差 | 完全不行 |
|---|---|---|---|
| 白雜訊（頻帶內訊雜比） | 6 dB／10 dB 以上 | 4 dB：90%／8 dB：6% | 2 dB／6 dB |
| 回音（RT60 0.5 秒），直達聲對回音的比值 | −6 dB／+5 dB 以上 | −10 dB：88%、−15 dB：46%／0 dB：95%、−3 dB：15% | —／−6 dB |
| 殘響時間（直達聲和回音一樣大） | 0.8 秒／0.3 秒以內 | 1.2 秒：97%／0.5 秒：92% | 2.0 秒：6%／0.8 秒 |
| 錄音端時鐘誤差 | ±400 ppm | −1000 ppm：100%／0% | +1000 ppm、±2000 ppm |
| 高頻被切掉（頻帶上緣 7.0 kHz） | 切在 5.5 kHz 以上 | 5.0 kHz：67%／66% | 4.0 kHz |
| 低頻被切掉（頻帶下緣 1.0 kHz） | 切到 3.0 kHz 仍正常（只試到這裡） | | |
| 破音（削波位準，以訊號 RMS 的倍數計） | 0.3 倍／1.0 倍 | —／0.7 倍：91%、0.5 倍：20% | —／0.3 倍 |
| 每秒爆音次數（3 毫秒、比訊號大 20 dB） | 10／2 | 20：99%／5：97%、10：82% | —／20：24% |
| 每分鐘掉音次數（每次少 50 毫秒） | 0 | 6：82%、15：62%、30：33% | 120：6% |

掉音是弱點：少掉 50 毫秒會讓後面全部錯位，那一段（1.7 秒）剩下的部分就沒了。

**為什麼符號是 171 毫秒。** 第一版用 21 毫秒的符號、5 毫秒的循環字首，回音一旦和直達聲一樣大就完全收不到。
同一個數據機、四種符號長度、RT60 0.45 秒（QPSK／16-QAM 送達比例）：

| 符號＋字首 | QPSK 速率 | 直達聲比回音 +5 dB | 0 dB | −5 dB | −10 dB |
|---|---|---|---|---|---|
| 21＋5 毫秒 | 465 B/s | 100%／0% | 0%／0% | 0%／0% | 0%／0% |
| 43＋11 毫秒 | 456 B/s | 100%／0% | 10%／0% | 0%／0% | 0%／0% |
| 85＋21 毫秒 | 424 B/s | 100%／98% | 99%／0% | 2%／0% | 0%／0% |
| 171＋43 毫秒（採用） | 372 B/s | 100%／100% | 100%／96% | 100%／8% | 85%／0% |

**怎麼選 QPSK 或 16-QAM。** 沒有回傳通道，所以發送端要先決定。規則：用 QPSK；除非先錄一段測試、
`chirpix decode` 回報每個載波 12 dB 以上，才用 16-QAM（速度加倍）。在「good」通道（回報 20 dB）會選 16-QAM，
75 秒的圖從 26.4 dB 變成 29.2 dB。這個規則偏保守：「fair」通道（10 dB）它選 QPSK，但其實 16-QAM 在那裡也收得到。

**只看影像編碼。** kodim23 在 24 kB 時 38.2 dB、9.1 kB 時 33.6 dB、0.4 kB 時 22.5 dB。沒有和 JPEG 2000 或其他編碼器比較。

## 限制

- **沒有真實空氣傳輸的結果。** 模擬沒有包含喇叭失真、錄音軟體的自動音量與降噪、有損音訊壓縮、裝置移動。其中任何一項都可能是主因。
- 頻帶（1–7 kHz）是依照對小喇叭和錄音軟體的一般認識選的，沒有實際量過任何裝置。
- 聽得到而且不好聽：6 kHz 寬、像雜訊的聲音，每 1.7 秒一聲滑音。
- 速度慢：QPSK 每秒 372 位元組。一張 768x512 的照片 75 秒只能送 9 kB，平均約 26 dB。
- 第一張圖很粗（0.4 kB），而且要收滿兩段：通常 4–5 秒，有時更久。
- 符號很長，所以怕移動，也怕超過 ±600 ppm 的時鐘誤差。
- 錄音掉音會損失那一段剩下的部分。沒有做即時接收、超音波模式。
- SSTV 對照組只有一種模式（Robot 36）和一個接收端（我自己寫的）。它的時序和兩個獨立實作相符，但解碼器只在乾淨的聲音上
  和別人的解碼器比過，平滑程度也是為了 PSNR 調的（用內建測試圖）。比較時兩者平均功率相同；SSTV 是定振幅訊號，
  在同樣的峰值限制下其實可以大聲約 7.6 dB。人眼看一張有雜訊的 SSTV 圖，可能比「和原尺寸原圖算 PSNR、SSIM」寬容。
- 多聲道錄音只讀第一個聲道；只吃 WAV 檔。
- 邊長超過 1024 像素的圖片會先縮小再編碼。

## 相關作品

- [quiet](https://github.com/quiet/quiet)（建立在 liquid-dsp 上）和 [amodem](https://github.com/romanz/amodem)
  是可以實際使用的 OFDM／QAM 聲音數據機；[ggwave](https://github.com/ggerganov/ggwave) 是穩健的多音 FSK 協定，
  搭配 Reed-Solomon 碼；[minimodem](https://github.com/kamalmostafa/minimodem) 實作了經典的 FSK 標準。
  它們可靠地傳位元組或檔案。chirpix 比它們都慢、也沒有它們經過的實戰驗證；多出來的是數據機上面那兩層的搭配。
- [Fldigi](http://www.w1hkj.com/) 和類比 SSTV 是業餘無線電用音訊通道傳圖的方式。SSTV 遇到雜訊會漸漸變差而不是整張失敗，
  但每張圖的時間固定，也沒有「中途加入」的概念。這裡把 Robot 36 當成對照組量過；用
  [pySSTV](https://github.com/dnet/pySSTV) 和 [sstv](https://pypi.org/project/sstv/) 套件驗證。
- 噴泉碼：LT 碼（Luby 2002）、Raptor／RaptorQ（RFC 6330）。用擴展視窗做不等保護出自 Sejdinovic、Vukobratovic、
  Doufexi、Senk、Piechocki 的論文〈Expanding window fountain codes for unequal error protection〉
  （IEEE Trans. Commun., 2009），論文裡也拿它搭配漸進式視訊。把它放在漸進式影像編碼下面是他們的想法，不是我的。
- 嵌入式小波編碼：EZW（Shapiro 1993）、SPIHT（Said、Pearlman 1996）、EZBC、JPEG 2000。這裡的編碼器是這個家族裡的小成員。

就我所知，沒有其他開源專案把擴展視窗噴泉碼和漸進式編碼放在聲音數據機上、並且報告「畫質對聆聽時間、對開始時間」的關係；
我沒有做徹底的搜尋，而且每一個零件都是標準技術。

## 建置與測試

```
make build     cargo build --release
make test      單元測試（每個零件）＋ 經過模擬通道的整合測試
make lint      rustfmt、clippy -D warnings、shell 語法檢查
make demo      用產生的測試圖做快速報告（CI 跑的就是這個）
make report    完整報告（有先執行 make data 就用 data/ 裡的照片）
make loopback  僅 macOS：經過 BlackHole 虛擬音效裝置，不發出聲音；沒有裝置或權限就略過
make sstv-check  Robot 36 對照 pySSTV 0.5.9 和 sstv 0.2.0，裝在 out/ 底下的 Python 虛擬環境裡
```

CI 在每個推上去的分支都會跑：Linux 上跑建置、lint、測試、快速報告和 SSTV 對照檢查，macOS 上跑測試。測試圖片在測試時產生，沒有提交任何二進位檔。

## 授權

MIT。`make data` 下載的柯達照片不屬於這個專案。
