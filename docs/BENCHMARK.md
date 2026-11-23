# Benchmark

How fast a search of the saved file lists is, measured on generated names so anyone can reproduce it.

## Run it

```powershell
dotnet run -c Release --project tools/IndexBench
```

It builds an in-memory index of 1,000,000 and then 5,000,000 generated names. For each size it runs every query in the set once (**First**), then 50 more times, and reports the 50th, 95th and 99th percentile. It prints the machine and a Markdown table, which is pasted below as printed. `--names 250000,2000000` picks other sizes; `--runs 20` changes the number of timed runs.

### What it measures, and what it leaves out

- **Names.** The names come from a fixed seed, so every run uses the same ones. They are a mix like an engineering share:
  - 40% drawing codes such as `AB-C-0704_Page12.pdf` or `KD-M-3321 Rev C.dwg`
  - 30% office files of one to four words with spaces, underscores or dashes, some with dates
  - 20% code files
  - 10% scans and photos

  A few words are accented or CJK (`Überprüfung`, `設計図`). Folders hold 20 to 400 files each, three levels below a share root.
- **Build.** The time to build the index from names already in memory. This is the step after the saved list (a SQLite shard) has been read from disk. Reading the shard from disk and reading a share are not included. For a live read of a real share, see `tools/LiveBench`.
- **Memory.** Managed heap growth after the index is built, with a full collection before and after.
- **Query.** The time `Searcher.Search` takes, which is what the panel waits on before drawing rows. It includes ranking the top results and finding files whose folder name matches. Drawing the rows on screen is not included. Each query searches one volume and hides no file types.
- **The query set**, which covers the kinds of typing that cost the most and the least:

  | Label | Query | Why |
  |---|---|---|
  | short prefix | `inv` | three letters, matches about one name in ten |
  | mid-string | `0704_page` | a code from the middle of a name, few matches |
  | extension filter | `ext:dwg 07` | a type filter plus two digits |
  | no match | `qzxjvw` | the scan alone, nothing to rank |
  | common substring | `e` | one letter: every name matches, through its own name or its folder's |

- **Disk type** doesn't matter, because everything searched is in memory.

## Results

AMD Ryzen 9 5950X 16-Core Processor, 32 logical cores, 64 GB RAM, Windows 10.0.26300.0, .NET 10.0.14, no debugger, 2026-11-21. `files` 1.7.4.

| Names | Build | Memory | Query | First | p50 | p95 | p99 | Matched |
|---:|---:|---:|---|---:|---:|---:|---:|---:|
| 1,000,000 | 0.5 s | 82 MB | short prefix `inv` | 37.9 ms | 8.1 ms | 11.6 ms | 19.1 ms | 94,878 |
| 1,000,000 | 0.5 s | 82 MB | mid-string `0704_page` | 2.9 ms | 1.1 ms | 1.5 ms | 13.2 ms | 4 |
| 1,000,000 | 0.5 s | 82 MB | extension filter `ext:dwg 07` | 4.6 ms | 4.4 ms | 5.2 ms | 5.7 ms | 3,013 |
| 1,000,000 | 0.5 s | 82 MB | no match `qzxjvw` | 0.9 ms | 0.5 ms | 0.6 ms | 1.0 ms | 0 |
| 1,000,000 | 0.5 s | 82 MB | common substring `e` | 32.5 ms | 23.7 ms | 34.7 ms | 39.1 ms | 1,000,000 |
| 5,000,000 | 1.3 s | 412 MB | short prefix `inv` | 17.9 ms | 18.3 ms | 31.6 ms | 35.5 ms | 490,997 |
| 5,000,000 | 1.3 s | 412 MB | mid-string `0704_page` | 5.6 ms | 5.2 ms | 5.8 ms | 5.8 ms | 52 |
| 5,000,000 | 1.3 s | 412 MB | extension filter `ext:dwg 07` | 15.4 ms | 13.9 ms | 17.2 ms | 23.8 ms | 17,314 |
| 5,000,000 | 1.3 s | 412 MB | no match `qzxjvw` | 5.5 ms | 4.6 ms | 5.1 ms | 5.4 ms | 0 |
| 5,000,000 | 1.3 s | 412 MB | common substring `e` | 116.4 ms | 120.4 ms | 135.6 ms | 137.3 ms | 5,000,000 |

**How to read it:**
- The scan itself takes about half a millisecond per million names (the **no match** row).
- A query with few matches stays within a few milliseconds.
- Most of the cost is ranking many matches. A one- or three-letter query that matches a large share of the names takes tens of milliseconds at 1M and over 100 ms at 5M.
- **First** at 1M includes compiling the search code on first use. At 5M it doesn't, because the 1M run warmed it up.

## Other tools

Fill these in only from runs on the same machine, with the same names and queries. Name each tool's version and the date of the run. A figure a tool publishes about itself goes in **Their own claim**, kept apart from what was measured here.

| Tool | Version | Date | 1M p50 `0704_page` | 1M p50 `e` | Memory at 1M | Their own claim |
|---|---|---|---:|---:|---:|---|
| files | 1.7.4 | 2026-11-21 | 1.1 ms | 23.7 ms | 82 MB | — |
| Everything | | | | | | |
| goz | | | | | | |
| kensaku | | | | | | |
| TNDexplorer | | | | | | |

These tools don't read a generated list. To compare them, write the same names to disk as empty files with `dotnet run -c Release --project tools/IndexBench -- --write D:ench --names 1000000`. Then let each tool index that folder and time the same queries in each tool.

## When to run it again

Run the benchmark again, and replace the table, for any release that changes `Core/VolumeIndex.cs`, `Core/Query.cs`, `Core/TopK.cs` or `Core/Searcher.cs`.
