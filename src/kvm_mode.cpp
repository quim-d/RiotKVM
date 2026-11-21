#include "kvm_mode.h"
#include "hid_dev.h"
#include <Preferences.h>

namespace kvmmode {
namespace {
uint8_t cur = C;
volatile int8_t pendingMode = -1;
}

void load() {
  Preferences p;
  if (p.begin("kvm", true)) { uint8_t m = p.getUChar("mode", C); p.end(); if (m <= C) cur = m; }
}
uint8_t current() { return cur; }
bool hid() { return cur == B || cur == C; }
bool sd() { return cur == A || cur == C; }
uint16_t pid() { return cur == A ? 0x0010 : cur == B ? 0x0011 : 0x0001; }
char letter() { return "ABC"[cur]; }

bool request(uint8_t m) { if (m > C) return false; pendingMode = m; return true; }
void requestNext() { pendingMode = (cur + 1) % 3; }

void service() {
  int8_t m = pendingMode;
  if (m < 0) return;
  pendingMode = -1;
  if ((uint8_t)m == cur) return;
  kvmhid::releaseAllNow();           // soltar el estado antes de re-enumerar
  Preferences p;
  if (p.begin("kvm", false)) { p.putUChar("mode", (uint8_t)m); p.end(); }
  delay(80);
  esp_restart();
}
}  // namespace kvmmode
