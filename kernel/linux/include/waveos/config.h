/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: the configuration Linux code sees (forced in with -include).
 * Only what the ported drivers need is enabled; everything else reads as off.
 */
#ifndef _WAVEOS_CONFIG_H
#define _WAVEOS_CONFIG_H

/* x86 is little-endian (Linux code tests __LITTLE_ENDIAN). */
#define __LITTLE_ENDIAN 1234
#define CONFIG_X86 1
#define CONFIG_X86_64 1
#define CONFIG_64BIT 1
#define CONFIG_SMP 1
#define CONFIG_NR_CPUS 64
#define CONFIG_HZ 1000
#define CONFIG_PAGE_SHIFT 12
#define CONFIG_DRM 1
#define CONFIG_DRM_KMS_HELPER 1
#define CONFIG_VIRTIO 1
#define CONFIG_DRM_VIRTIO_GPU_KMS 1

#define IS_ENABLED(option) __waveos_is_defined(option)
#define IS_BUILTIN(option) __waveos_is_defined(option)
#define IS_MODULE(option) 0
#define IS_REACHABLE(option) __waveos_is_defined(option)
#define __waveos_arg_placeholder_1 0,
#define __waveos_take_second(ignored, val, ...) val
#define __waveos_is_defined(x) ___waveos_is_defined(x)
#define ___waveos_is_defined(val) ____waveos_is_defined(__waveos_arg_placeholder_##val)
#define ____waveos_is_defined(arg1_or_junk) __waveos_take_second(arg1_or_junk 1, 0)

/* Like Linux's compiler_types.h, the base definitions reach every file. */
#include <waveos/base.h>

#endif
