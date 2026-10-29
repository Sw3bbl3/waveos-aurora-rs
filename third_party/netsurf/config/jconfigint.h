/* libjpeg-turbo 2.1.5 internal configuration for NetSurf on WaveOS (jconfigint.h.in, filled in). */
#define BUILD  "waveos"
#undef inline
#define INLINE  __inline__ __attribute__((always_inline))
#define THREAD_LOCAL  __thread
#define PACKAGE_NAME  "libjpeg-turbo"
#define VERSION  "2.1.5"
#define SIZEOF_SIZE_T  8
#define HAVE_BUILTIN_CTZL
#if defined(__has_attribute)
#if __has_attribute(fallthrough)
#define FALLTHROUGH  __attribute__((fallthrough));
#else
#define FALLTHROUGH
#endif
#else
#define FALLTHROUGH
#endif
