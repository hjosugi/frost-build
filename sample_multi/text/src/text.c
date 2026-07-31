#include "text.h"

#include <stdio.h>

#include "core.h"

void frost_text_banner(char *out, unsigned long cap) {
    snprintf(out, (size_t)cap, "frost %s", frost_core_version());
}
