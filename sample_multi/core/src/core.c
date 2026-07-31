#include "core.h"

/* Produced by //:gen_version. Reaching it from another package is the point:
 * the include path comes from that target's `includes`, and the header is an
 * order-only input of this compile, which is what `query owners` reports. */
#include "version.h"

const char *frost_core_version(void) {
    return FROST_SAMPLE_MULTI_VERSION;
}

int frost_core_add(int a, int b) {
    return a + b;
}
