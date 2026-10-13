/* check-only 垫片（M0 设计 §2.3）：clang 自带的 immintrin.h → xmmintrin.h → mm_malloc.h
 * 里有 `#include <stdlib.h>`；`-nostdlibinc` 后无目标 sysroot ⇒ x86_64 目标的 ring
 * C 前端会报 'stdlib.h' file not found。本垫片只**声明不定义**，让 C 前端过完。
 *
 * 纪律（三条，违反即产出一个「无 libc 的 ring 对象」）：
 *   1. 本头文件与 `-nostdlibinc` **只用于 check-only 门**（ci.yml 的交叉 check job +
 *      tools/ci-local.sh 步骤 3 的 check 档）；
 *   2. **绝不**进 `.cargo/config.toml` 的 `[env]`、**绝不**进任何真实构建路径
 *      （真构建 = tools/build-app-core.sh / tier 出包 / quic-ab 的 size 档，一律真 sysroot）；
 *   3. 对象永不参与链接（check 不链接）。
 * 门禁：ci.yml 有一条 fail-closed 断言——`cargo check -v` 输出里每条含 `-nostdlibinc`
 *   的命令行都必须来自 ring（防将来新 C 依赖静默继承，设计 §2.6 / §8.2 R-K）。
 */
#ifndef HW_CHECK_SHIM_STDLIB_H
#define HW_CHECK_SHIM_STDLIB_H

#include <stddef.h>

void *malloc(size_t __size);
void free(void *__ptr);

/* x86_64-pc-windows-msvc 交叉 check（WIN 批，2026-10-13）：clang `--target=…msvc`
 * 下 ring 的 internal.h（`#if defined(_WIN32)` 臂）调 `_byteswap_ulong/_byteswap_uint64`
 * （MSVC 的 stdlib.h 面）——同样只声明给 C 前端过（check 不链接）。clang 内建直供，
 * 非 _WIN32 编译面（OHOS/musl 交叉 check）不受影响。 */
#if defined(_WIN32) && defined(__clang__)
#define _byteswap_ulong(v) __builtin_bswap32(v)
#define _byteswap_uint64(v) __builtin_bswap64(v)
#endif

#endif /* HW_CHECK_SHIM_STDLIB_H */
