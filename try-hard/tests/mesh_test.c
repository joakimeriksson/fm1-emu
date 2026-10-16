/* SPDX-License-Identifier: GPL-3.0-only */
/* The gears really mesh: through a whole turn of the train, two meshing gears never cover the same point
 * (no tooth runs into a tooth), and their teeth reach into each other's tooth ring (engaged, not just near).
 * Built by try-hard/build.sh --test, in host mode (Dinghy's tests/host.c as the FM-1). */
#define main host_main
#include "dinghy.c"
#undef main

static int solid(const disc_t *d, int32_t x, int32_t y)    /* (x, y) in Q4 */
{
    int32_t dx = x - ax_x[d->ax], dy = y - ax_y[d->ax];
    int32_t r = (int32_t)isqrt((uint32_t)(dx * dx + dy * dy)) / 4;
    return disc_px(d, r, iatan2(dy, dx)) != 0;
}

int main(void)
{
    uint32_t k, step, a, bad = 0;
    gears_init();
    for (k = 0; k < NAX - 1u; k++) {
        const disc_t *da = &disc[MESH[k].i], *db = &disc[MESH[k].j];
        uint32_t overlap = 0, engaged = 0;
        for (step = 0; step < 2000u; step++) {          /* the master angle over a full cycle of the train */
            int32_t mx, my, x, y;
            Qm = (uint32_t)((uint64_t)step * QWRAP / 2000u);
            for (a = 0; a < NAX; a++)
                ax_th[a] = ((uint32_t)((int32_t)Qm / 120 * K[a] + (int32_t)(Qm % 120u) * K[a] / 120) + ax_c[a]) & 0xFFFFu;
            /* the pitch point: on the line between the centres, at a's pitch radius */
            mx = ax_x[da->ax] + (ax_x[db->ax] - ax_x[da->ax]) * da->n / (da->n + db->n);
            my = ax_y[da->ax] + (ax_y[db->ax] - ax_y[da->ax]) * da->n / (da->n + db->n);
            for (y = my - 10 * Q4; y <= my + 10 * Q4; y += Q4 / 2)
                for (x = mx - 10 * Q4; x <= mx + 10 * Q4; x += Q4 / 2) {
                    int ca = solid(da, x, y), cb = solid(db, x, y);
                    int32_t dx = (x - ax_x[db->ax]) / 4, dy = (y - ax_y[db->ax]) / 4;   /* 1/4 px */
                    if (ca && cb) overlap++;
                    if (ca && dx * dx + dy * dy < (db->rt - 4) * (db->rt - 4)) engaged++;
                }
        }
        printf("mesh %u: disc %u (%u teeth) -> disc %u (%u teeth): %u points overlap, %u engaged\n",
               k, MESH[k].i, da->n, MESH[k].j, db->n, overlap, engaged);
        if (overlap || !engaged) bad++;
    }
    printf(bad ? "FAIL\n" : "ok: every mesh turns clear and engaged\n");
    return bad ? 1 : 0;
}
