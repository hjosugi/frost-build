#include <stdio.h>

#include "render.h"
#include "text.h"

int main(void) {
    char banner[32];
    frost_text_banner(banner, sizeof banner);
    printf("%s: %d\n", banner, frost_render_answer());
    return 0;
}
