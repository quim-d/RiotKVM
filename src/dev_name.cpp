#include "dev_name.h"
#include "ble_link.h"
#include <Preferences.h>

namespace devname {
namespace {
portMUX_TYPE mux = portMUX_INITIALIZER_UNLOCKED;
char cur[MAX_LEN + 1] = "RiotKVM";
volatile uint32_t g = 0;
bool okChar(uint8_t c) { return isalnum(c) || c == '.' || c == '_' || c == '-'; }
}

bool valid(const uint8_t* s, size_t len) {
  if (len < 1 || len > MAX_LEN) return false;
  for (size_t i = 0; i < len; i++) if (!okChar(s[i])) return false;
  return true;
}

void load() {
  Preferences p;
  if (!p.begin("kvm", true)) return;
  char b[MAX_LEN + 2] = "";
  size_t n = p.isKey("name") ? p.getString("name", b, sizeof b) : 0;   // getString cuenta el 0 final
  p.end();
  if (n) n--;
  if (valid((const uint8_t*)b, n)) { memcpy(cur, b, n); cur[n] = 0; }
}

void get(char* out, size_t n) {
  if (!n) return;
  portENTER_CRITICAL(&mux);
  strlcpy(out, cur, n);
  portEXIT_CRITICAL(&mux);
}
uint32_t gen() { return g; }

bool set(const uint8_t* s, size_t len) {
  if (len == 0) { s = (const uint8_t*)DEFAULT_NAME; len = strlen(DEFAULT_NAME); }
  if (!valid(s, len)) return false;
  char b[MAX_LEN + 1]; memcpy(b, s, len); b[len] = 0;
  portENTER_CRITICAL(&mux);
  bool same = !strcmp(b, cur);
  if (!same) strcpy(cur, b);
  portEXIT_CRITICAL(&mux);
  if (same) return true;
  Preferences p;
  if (p.begin("kvm", false)) {
    if (!strcmp(b, DEFAULT_NAME)) p.remove("name"); else p.putString("name", b);
    p.end();
  }
  g++;
  blelink::nameChanged();   // nombre GAP + scan response nuevos, sin reiniciar
  return true;
}
}  // namespace devname
