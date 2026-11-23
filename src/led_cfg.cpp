#include "led_cfg.h"
#include <Preferences.h>

namespace ledcfg {
namespace {
portMUX_TYPE mux = portMUX_INITIALIZER_UNLOCKED;
Cfg cur{AUTO, 0, 255, 70, 35};      // defecto del color fijo: verde de la paleta al 35 %
volatile uint32_t g = 0;
uint32_t dirtyAt = 0; bool dirty = false;
}

void load() {
  Preferences p;
  if (!p.begin("kvm", true)) return;
  Cfg c{p.getUChar("led_m", AUTO), 0, 0, 0, p.getUChar("led_br", cur.bright)};
  uint32_t rgb = p.getULong("led_rgb", (uint32_t)cur.r << 16 | cur.g << 8 | cur.b);
  p.end();
  c.r = rgb >> 16; c.g = rgb >> 8; c.b = rgb;
  if (c.mode <= OFF && c.bright <= 100) cur = c;
}

Cfg get() { portENTER_CRITICAL(&mux); Cfg c = cur; portEXIT_CRITICAL(&mux); return c; }
uint32_t gen() { return g; }

bool set(const Cfg& c) {
  if (c.mode > OFF || c.bright > 100) return false;
  portENTER_CRITICAL(&mux);
  bool same = !memcmp(&c, &cur, sizeof c);
  cur = c;
  portEXIT_CRITICAL(&mux);
  if (same) return true;
  g++; dirty = true; dirtyAt = millis();
  return true;
}

void service() {
  if (!dirty || millis() - dirtyAt < 1000) return;
  dirty = false;
  Cfg c = get();
  Preferences p;
  if (p.begin("kvm", false)) {
    p.putUChar("led_m", c.mode); p.putULong("led_rgb", (uint32_t)c.r << 16 | c.g << 8 | c.b); p.putUChar("led_br", c.bright);
    p.end();
  }
}

void describe(char* b, size_t n) {
  Cfg c = get();
  if (c.mode == AUTO) snprintf(b, n, "auto");
  else if (c.mode == OFF) snprintf(b, n, "off");
  else snprintf(b, n, "%02X%02X%02X@%u", c.r, c.g, c.b, c.bright);
}
}  // namespace ledcfg
