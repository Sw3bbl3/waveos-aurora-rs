/* SPDX-License-Identifier: MIT */
/* WaveOS LinuxKPI: tracepoints are compiled out. */
#include <waveos/kernel.h>

#define trace_dma_fence_emit(f) do { (void)(f); } while (0)
