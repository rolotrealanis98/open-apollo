"""Userspace stand-ins for the kernel API the plug-in host files use.

The plug-in host sources (ua_resource_pool.c, ua_dsp_xact.c,
ua_plugin_*.c, ua_strips.c) are compiled whole by the host tests: their
#include lines are dropped and this prefix supplies the types, locks,
allocators (with fault injection) and helpers they call.  Device access is
never stubbed here: each test supplies its own fake ring or fake device.
"""
import re

import csrc

PREFIX = r'''
#include <assert.h>
#include <errno.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef uint8_t u8;
typedef uint16_t u16;
typedef uint32_t u32;
typedef uint64_t u64;
typedef int8_t s8;
typedef int32_t s32;
typedef int64_t s64;
typedef uint8_t __u8;
typedef uint32_t __u32;
typedef uint64_t __u64;
typedef int32_t __s32;
typedef uint32_t __le32;
typedef uint64_t dma_addr_t;

#define U32_MAX UINT32_MAX
#define S64_MAX INT64_MAX
#define GFP_KERNEL 0
#define __user
#define __maybe_unused __attribute__((unused))
#define __printf(a, b) __attribute__((format(printf, a, b)))
#define BIT(n) (1UL << (n))
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define min(a, b) ((a) < (b) ? (a) : (b))
#define max(a, b) ((a) > (b) ? (a) : (b))
#define min_t(t, a, b) ((t)(a) < (t)(b) ? (t)(a) : (t)(b))
#define max_t(t, a, b) ((t)(a) > (t)(b) ? (t)(a) : (t)(b))
#define DIV_ROUND_UP(n, d) (((n) + (d) - 1) / (d))
#define ALIGN(x, a) (((x) + (a) - 1) & ~((__typeof__(x))(a) - 1))
#define PAGE_SIZE 4096
#define PAGE_ALIGN(x) (((x) + 4095) & ~(size_t)4095)
#define cpu_to_le32(x) (x)
#define le32_to_cpu(x) (x)
#define lower_32_bits(x) ((u32)(x))
#define upper_32_bits(x) ((u32)((u64)(x) >> 32))
#define READ_ONCE(x) (x)
#define WRITE_ONCE(x, v) ((x) = (v))
#define WARN_ON_ONCE(c) ({ int __w = !!(c); assert(!__w); __w; })
#define WARN_ON(c) WARN_ON_ONCE(c)
#define BUILD_BUG_ON(c) _Static_assert(!(c), #c)
#define dma_wmb() do { } while (0)
#define dma_rmb() do { } while (0)
#define wmb() do { } while (0)
#define module_param(n, t, p)
#define module_param_cb(n, o, a, p)
#define MODULE_PARM_DESC(n, d)
#define EXPORT_SYMBOL_GPL(s)
#define likely(x) (x)
#define unlikely(x) (x)
#define fallthrough __attribute__((fallthrough))

static inline u32 get_unaligned_le32(const void *p)
{
	const u8 *b = p;

	return b[0] | (u32)b[1] << 8 | (u32)b[2] << 16 | (u32)b[3] << 24;
}

static inline void put_unaligned_le32(u32 v, void *p)
{
	u8 *b = p;

	b[0] = v; b[1] = v >> 8; b[2] = v >> 16; b[3] = v >> 24;
}

typedef struct { s64 value; } atomic64_t;
#define ATOMIC64_INIT(v) { v }
static inline s64 atomic64_read(atomic64_t *a) { return a->value; }
static inline s64 atomic64_cmpxchg(atomic64_t *a, s64 old, s64 next)
{
	s64 v = a->value;

	if (v == old)
		a->value = next;
	return v;
}
typedef struct { int counter; } atomic_t;
#define atomic_read(a) ((a)->counter)
#define atomic_set(a, v) ((a)->counter = (v))

/* Locks: assert the discipline, never block */
struct mutex { int held; };
static int mutexes_held __maybe_unused;
static inline void mutex_init(struct mutex *m) { m->held = 0; }
static inline void mutex_lock(struct mutex *m)
{
	assert(!m->held);
	m->held = 1;
	mutexes_held++;
}
static inline int mutex_lock_killable(struct mutex *m)
{
	mutex_lock(m);
	return 0;
}
static inline int mutex_trylock(struct mutex *m)
{
	if (m->held)
		return 0;
	mutex_lock(m);
	return 1;
}
static inline void mutex_unlock(struct mutex *m)
{
	assert(m->held);
	m->held = 0;
	mutexes_held--;
}
static inline bool mutex_is_locked(struct mutex *m) { return m->held; }
#define lockdep_assert_held(m) assert((m)->held)

/* Allocators: count objects, fail the Nth call on demand */
static int k_objects __maybe_unused, k_allocs __maybe_unused, k_fail_at __maybe_unused;
static void *k_alloc(size_t n)
{
	void *p;

	if (k_fail_at && ++k_allocs == k_fail_at)
		return NULL;
	p = calloc(1, n ? n : 1);
	if (p)
		k_objects++;
	return p;
}
static inline void *kzalloc(size_t n, int f) { (void)f; return k_alloc(n); }
static inline void *kmalloc(size_t n, int f) { (void)f; return k_alloc(n); }
static inline void *kcalloc(size_t n, size_t s, int f) { (void)f; return k_alloc(n * s); }
static inline void *kvmalloc(size_t n, int f) { (void)f; return k_alloc(n); }
static inline void *kvzalloc(size_t n, int f) { (void)f; return k_alloc(n); }
static inline void *kmemdup(const void *s, size_t n, int f)
{
	void *p = kmalloc(n, f);

	if (p)
		memcpy(p, s, n);
	return p;
}
static inline void kfree(const void *p)
{
	if (p) {
		assert(k_objects > 0);
		k_objects--;
		free((void *)p);
	}
}
#define kvfree(p) kfree(p)

/* Logging: one buffer the tests search */
static char k_log[1 << 17] __maybe_unused;
static size_t k_loglen __maybe_unused;
static void k_logf(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
static void k_logf(const char *fmt, ...)
{
	va_list a;
	int r;

	va_start(a, fmt);
	r = vsnprintf(k_log + k_loglen, sizeof(k_log) - k_loglen, fmt, a);
	va_end(a);
	if (r > 0)
		k_loglen += (size_t)r;
	if (k_loglen >= sizeof(k_log) - 256)
		k_loglen = 0;
}
#define K_HAS(s) (strstr(k_log, (s)) != NULL)
static inline void k_log_reset(void) { k_loglen = 0; k_log[0] = 0; }
#define dev_info(d, ...) ((void)(d), k_logf(__VA_ARGS__))
#define dev_warn(d, ...) ((void)(d), k_logf(__VA_ARGS__))
#define dev_err(d, ...) ((void)(d), k_logf(__VA_ARGS__))
#define dev_dbg(d, ...) ((void)(d), k_logf(__VA_ARGS__))
#define dev_info_ratelimited dev_info
#define dev_warn_ratelimited dev_warn
#define dev_info_once dev_info

static inline int vscnprintf(char *b, size_t n, const char *fmt, va_list a)
{
	int r = vsnprintf(b, n, fmt, a);

	return r < 0 ? 0 : (size_t)r >= n ? (int)n - 1 : r;
}
static inline int scnprintf(char *b, size_t n, const char *fmt, ...)
	__attribute__((format(printf, 3, 4)));
static inline int scnprintf(char *b, size_t n, const char *fmt, ...)
{
	va_list a;
	int r;

	va_start(a, fmt);
	r = vscnprintf(b, n, fmt, a);
	va_end(a);
	return r;
}
static inline void strscpy(char *d, const char *s, size_t n)
{
	size_t l = strlen(s);

	if (l >= n)
		l = n - 1;
	memcpy(d, s, l);
	d[l] = 0;
}

/* list.h, the parts used */
struct list_head { struct list_head *next, *prev; };
#define LIST_HEAD_INIT(n) { &(n), &(n) }
static inline void INIT_LIST_HEAD(struct list_head *l) { l->next = l->prev = l; }
static inline void list_add_tail(struct list_head *n, struct list_head *h)
{
	n->prev = h->prev;
	n->next = h;
	h->prev->next = n;
	h->prev = n;
}
static inline void list_del(struct list_head *n)
{
	n->prev->next = n->next;
	n->next->prev = n->prev;
	n->next = n->prev = NULL;
}
static inline void list_del_init(struct list_head *n)
{
	list_del(n);
	INIT_LIST_HEAD(n);
}
static inline bool list_empty(const struct list_head *h) { return h->next == h; }
#define container_of(p, t, m) ((t *)((char *)(p) - offsetof(t, m)))
#define list_entry(p, t, m) container_of(p, t, m)
#define list_first_entry(h, t, m) list_entry((h)->next, t, m)
#define list_for_each_entry(p, h, m) \
	for (p = list_entry((h)->next, __typeof__(*p), m); &p->m != (h); \
	     p = list_entry(p->m.next, __typeof__(*p), m))
#define list_for_each_entry_safe(p, n, h, m) \
	for (p = list_entry((h)->next, __typeof__(*p), m), \
	     n = list_entry(p->m.next, __typeof__(*p), m); &p->m != (h); \
	     p = n, n = list_entry(n->m.next, __typeof__(*n), m))

/* Time: a counter the tests advance; sleeping advances it */
static unsigned long jiffies __maybe_unused = 100000;
static unsigned int k_sleeps __maybe_unused;
#define HZ 1000
#define time_before(a, b) ((long)((a) - (b)) < 0)
#define time_after(a, b) ((long)((b) - (a)) < 0)
#define time_after_eq(a, b) ((long)((a) - (b)) >= 0)
#define msecs_to_jiffies(m) ((unsigned long)(m))
#define jiffies_to_msecs(j) ((unsigned int)(j))
'''


def body(name, begin=None, end=None):
    """driver/<name> without its #include lines (or the part between the
    begin and end marks)."""
    text = csrc.source(name)
    if begin:
        text = csrc.between(text, begin, end)
    return re.sub(r'^#include .*$', '', text, flags=re.M)


def header(name):
    """A header's declarations without its include guard and includes."""
    text = csrc.source(name)
    guard = re.compile(r'^#(ifndef|define)\s+UA_\w+_H\s*$|^#endif\s*/\*\s*UA_\w+_H\s*\*/')
    return '\n'.join(line for line in text.splitlines()
                     if not line.startswith('#include') and not guard.match(line))
