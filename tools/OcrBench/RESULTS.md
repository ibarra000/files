# Correcting text recognition: results (issue #62)

Measured with this tool on 2026-11-13: `dotnet run -c Release --project tools/OcrBench -- --codes 80 --device vulkan --llm <model.gguf>...`.
It is not part of the app or the installer.

## What was measured

- **320 crops** of 80 codes made of look-alike characters (0/O, 1/I/l, 5/S, 8/B, 2/Z, 6/G) in the shapes of drawing and part numbers, such as `OI-N-5430`, `BI2005_RevB` and `0856-TQA-88E`. Each was drawn at screen size in a line of text, in Segoe UI, Consolas, Arial, Courier New or Tahoma at 11, 13 or 15 px, light or dark. A third were saved as JPEG at quality 35. Each crop was then enlarged with bicubic scaling, as the app enlarges its screen capture, and read with **Windows OCR** and **PaddleOCR**. The word under the pointer was taken as `ScreenText.Nearest` takes it.
- **An index of 200,000 names.** Every code is there as a PDF, alongside many codes of the same shapes. For a fifth of the codes there is also a twin that differs in one look-alike character, so guessing at characters can find the wrong file.
- **Corrections** of what was read:
  - *as read*: what the app does today.
  - *look-alike swap, checked in the index*: no model. If the read finds nothing, look-alike characters are tried, fewest changes first (at most 3 changes and 512 tries), and the first variant the index has is kept.
  - *model*: a small local language model, given a few examples and asked for the corrected code. Greedy decoding, GGUF Q4_K_M, through LLamaSharp 0.30.
  - *model, checked in the index*: the model's answer is kept only if the index has it; otherwise the look-alike swap is used.
- **Opens it**: a search for the result lists the code's file first, so Enter opens it.

## Results

Accuracy is the same on CPU and GPU, apart from a few reads where floating-point differences change the greedy answer (see the last table). These are the GPU run's numbers.

| Engine | Correction | Opens it | Read exactly | Time per read |
|---|---|---:|---:|---:|
| PaddleOCR | as read | 82.2% | 79.4% | – |
| PaddleOCR | **look-alike swap, checked in the index** | **95.0%** | 92.2% | **1.3 ms** (p95 3.4 ms) |
| PaddleOCR | SmolLM2-360M | 64.1% | 61.6% | 100 ms on the GPU |
| PaddleOCR | Qwen2.5-0.5B | 62.2% | 59.4% | 62 ms on the GPU |
| PaddleOCR | Qwen2.5-1.5B | 82.5% | 79.4% | 97 ms on the GPU |
| PaddleOCR | SmolLM2-360M, checked in the index | 95.3% | 92.5% | 100 ms on the GPU |
| PaddleOCR | Qwen2.5-0.5B, checked in the index | 95.3% | 92.2% | 63 ms on the GPU |
| PaddleOCR | Qwen2.5-1.5B, checked in the index | 95.6% | 92.5% | 97 ms on the GPU |
| Windows OCR | as read | 43.4% | 38.1% | – |
| Windows OCR | **look-alike swap, checked in the index** | **71.6%** | 62.8% | **2.5 ms** (p95 17 ms) |
| Windows OCR | SmolLM2-360M | 35.6% | 32.5% | 92 ms on the GPU |
| Windows OCR | Qwen2.5-0.5B | 35.3% | 30.6% | 64 ms on the GPU |
| Windows OCR | Qwen2.5-1.5B | 46.2% | 40.9% | 98 ms on the GPU |
| Windows OCR | Qwen2.5-1.5B, checked in the index | 74.1% | 65.3% | 100 ms on the GPU |

Model speed, RTX 5060 Ti through Vulkan (every layer on the GPU) vs. the CPU. Models are 271 MB (SmolLM2-360M), 491 MB (Qwen2.5-0.5B) and 1,117 MB (Qwen2.5-1.5B) on disk:

| Model | CPU mean / p95 | GPU mean / p95 | Speed-up | Same answer on both |
|---|---:|---:|---:|---:|
| SmolLM2-360M | 374 / 426 ms | 96 / 115 ms | 3.9× | 620 of 640 |
| Qwen2.5-0.5B | 366 / 413 ms | 63 / 88 ms | 5.8× | 604 of 640 |
| Qwen2.5-1.5B | 666 / 749 ms | 97 / 120 ms | 6.9× | 635 of 640 |

CUDA was not measured: LLamaSharp's CUDA 13 backend needs the CUDA runtime (`cudart64_13.dll`, `cublas64_13.dll`), which this PC does not have. Vulkan needs only the graphics driver.

## Conclusion

- **On their own, the small models make things worse.** They mostly repeat what was read, and when they change it, they break about as many codes as they fix. Even Qwen2.5-1.5B only matches doing nothing. A model knows what codes *look like*, not which ones *exist*.
- **Checking against the index is what helps, and it needs no model.** The look-alike swap lifts PaddleOCR from 82% to 95% and Windows OCR from 43% to 72%. Adding a model before the check gains at most 0.6 points (Qwen2.5-1.5B with PaddleOCR) or 2.5 points (with Windows OCR), for 0.3–1.1 GB more in the installer and about 100 ms per read on a good GPU. On a PC without one it is 0.4–0.7 s.
- **Recommendation:** don't ship a language model. Instead, after recognition, run the look-alike swap checked in the index (`tools/OcrBench/Correctors.cs` `IndexSwap`): when what was read finds nothing, search for the closest variant that exists. It needs no download, adds about 1–3 ms, and covers most of the gain.

These are synthetic crops. Real screenshots (CAD viewers, scanned sheets) may differ, so a set of real crops with their true codes would be the next check.
