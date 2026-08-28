# FastCopy vs Explorer benchmark

Measured **2026-08-28** with FastCopy **1.0.2**.
测量日期 **2026-08-28**，FastCopy **1.0.2**。

On a same-volume HDD copy of a `node_modules` tree (many small files), FastCopy was about **12×** faster than Explorer paste for copy, and about **2×** faster for permanent delete.
同一分区、机械盘上的 `node_modules`（大量小文件）：复制（对照资源管理器粘贴）约快 **12 倍**，永久删除约快 **2 倍**。

## Dataset / 测试数据

| | |
| --- | --- |
| Tree | `node_modules` |
| Files / 文件 | 4019 |
| Directories / 目录 | 366 |
| Size / 大小 | 61.0 MiB |
| Average file / 平均文件 | ~15.5 KiB |

Source was read-only. Destinations were copies under a throwaway folder, then deleted.
源目录只读；复制到临时目录后再删除，不改动源。

## Environment / 环境

| | |
| --- | --- |
| OS | Windows 10 19045 |
| CPU | Intel Core i5-12600K (10 cores / 16 threads) |
| RAM | 64 GB |
| Volume | NTFS, same disk as the source |
| Disk | HGST HUH721212ALE601 12 TB HDD (7200 rpm) |
| FastCopy | 1.0.2 Release, `--workers 16` |

`--workers 16` matches the in-app default (logical CPU count, clamped 2–16).
`--workers 16` 与程序默认一致（逻辑处理器数，限制在 2–16）。

## Method / 方法

Explorer **copy/paste**: Ctrl+C only fills the clipboard. The I/O is paste, which uses Windows `IFileOperation` (the Explorer copy engine). This benchmark times `IFileOperation.CopyItem` + `PerformOperations` with `FOF_NO_UI` (no confirmation or progress dialog).
资源管理器**复制/粘贴**：Ctrl+C 只写入剪贴板，真正拷数据的是粘贴，走 `IFileOperation`。本次测量 `CopyItem` + `PerformOperations`，并加 `FOF_NO_UI`（无确认框、无进度窗）。

Explorer **delete**: `IFileOperation.DeleteItem` without `FOF_ALLOWUNDO` — the same as Shift+Delete (permanent, not Recycle Bin).
资源管理器**删除**：`DeleteItem` 且不加 `FOF_ALLOWUNDO`，等同 Shift+Delete（永久删除，不进回收站）。

FastCopy:

```text
fastcopy.exe --copy SOURCE DEST_FOLDER --workers 16
fastcopy.exe --delete PATH --permanent --yes --workers 16
```

Small files are copied with `ReadFile` / `WriteFile` (not `CopyFileEx`). Scan overlaps copy/delete. Progress events are coalesced to about 32 ms. Times include FastCopy process startup. After every copy, file count and byte size were checked against the source.
小文件用 `ReadFile` / `WriteFile` 拷（不用 `CopyFileEx`）。扫描与复制/删除重叠。进度事件约 32 ms 合并一次。FastCopy 耗时含进程启动。每次复制后核对文件数与字节数，须与源一致。

One warmup round (Explorer then FastCopy), then three measured rounds with alternating order: Explorer→FastCopy, FastCopy→Explorer, Explorer→FastCopy. About 1 second pause between operations.
先热身 1 轮（资源管理器再 FastCopy），再测 3 轮并交替顺序。操作之间暂停约 1 秒。

Recycle Bin was not compared. For a single top-level folder, both tools hand that path to the Windows Shell, so the interesting case is permanent delete.
未对比回收站：顶层只有一个文件夹时，两边都交给 Shell；更有对比意义的是永久删除。

## Results / 结果

Median of the three measured rounds (warmup excluded).
三轮实测的中位数（不含热身）。

| Operation / 操作 | Explorer | FastCopy | Speedup / 加速 |
| --- | ---: | ---: | ---: |
| Copy (paste) / 复制（粘贴） | 11.47 s | 0.93 s | **12.3×** |
| Permanent delete / 永久删除 | 2.02 s | 1.02 s | **2.0×** |

Throughput from the copy medians:
按复制中位数换算的吞吐：

| | Explorer | FastCopy |
| --- | ---: | ---: |
| MiB/s | 5.3 | 65.6 |
| Files/s / 文件/秒 | 350 | 4323 |

### Copy / 复制 (seconds)

| Round / 轮次 | Order / 顺序 | Explorer | FastCopy |
| --- | --- | ---: | ---: |
| Warmup / 热身 | Explorer → FastCopy | 11.80 | 0.99 |
| 1 | Explorer → FastCopy | 11.68 | 0.93 |
| 2 | FastCopy → Explorer | 11.47 | 1.08 |
| 3 | Explorer → FastCopy | 10.93 | 0.78 |
| Measured median / 实测中位数 | | **11.47** | **0.93** |
| Measured mean / 实测平均 | | 11.36 | 0.93 |

### Permanent delete / 永久删除 (seconds)

| Round / 轮次 | Explorer | FastCopy |
| --- | ---: | ---: |
| Warmup / 热身 | 2.19 | 1.03 |
| 1 | 1.96 | 0.84 |
| 2 | 2.13 | 1.31 |
| 3 | 2.02 | 1.02 |
| Measured median / 实测中位数 | **2.02** | **1.02** |
| Measured mean / 实测平均 | 2.04 | 1.05 |

### Earlier same-day run / 同日更早一次

Before the small-file path change (still `CopyFileEx`, scan then copy), the same tree copied in **1.48 s** median (7.9× Explorer). After the change it is **0.93 s**. Permanent delete stayed around **1 s** (already close to the metadata floor on this HDD).
改小文件路径之前（仍用 `CopyFileEx`，先扫完再拷），同一棵树复制中位数 **1.48 s**（比资源管理器快 7.9 倍）；改完后是 **0.93 s**。永久删除仍在 **1 s** 上下（这块机械盘上已经接近元数据下限）。

## Notes / 说明

- This dataset is many tiny files on a 7200 rpm HDD. That is the case FastCopy is meant for (concurrent buffered copy / concurrent `remove_file`). A single large file is usually limited by the disk and cache; FastCopy is not guaranteed to beat Explorer there.
  这是机械盘上的大量小文件，正是 FastCopy 针对的场景（并发缓冲拷贝 / 并发 `remove_file`）。单个大文件通常受磁盘和缓存限制，不保证快于资源管理器。
- Explorer was run without a UI. A visible paste/delete progress window can only be slower.
  资源管理器未显示进度窗；真正在界面里粘贴/删除只会更慢，不会更快。
- Windows antivirus real-time scanning was not turned off. Both sides saw the same machine policy.
  未关闭系统实时防护；两边同一台机器、同一套策略。
- Do not treat the 12× / 2× figures as a universal score. SSD vs HDD, file size mix, Defender, and worker count all move the result.
  12× / 2× 不是万能分数。SSD/HDD、文件大小分布、防护软件、线程数都会改变结果。
