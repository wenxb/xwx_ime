// rime_oracle: drive the locally installed librime (luna_pinyin_simp) as an
// oracle for the xwx_ime project.
//
// Protocol (per input line on stdin):
//   IN\t<raw input>
//   PREEDIT\t<preedit text, empty string allowed, line always present>
//   NCAND\t<total candidate count (full list, not one page)>
//   CAND\t<1-based index>\t<candidate text>   (at most 100 lines)
//   END
//
// Each input uses a fresh session (create -> simulate -> get_context ->
// iterate full candidate list -> destroy) so candidates never leak across
// inputs.

#include <cstdio>
#include <cstring>
#include <iostream>
#include <string>

#include "vendor/rime_api.h"

namespace {

constexpr const char* kSharedDataDir = "/usr/share/rime-data";
// IMPORTANT: never point this at ~/.local/share/fcitx5/rime.
constexpr const char* kUserDataDir =
    "/home/youngwen/Desktop/xwx_ime/tools/rime_oracle/.rime_user";

constexpr std::size_t kMaxCandidatesToPrint = 100;

// The xwx lexicon is simplified Chinese, so the oracle must emit simplified
// candidates too, otherwise candidate text cannot be compared directly.
constexpr const char* kSchemaId = "luna_pinyin_simp";

void InitRime() {
  // RIME_STRUCT zero-initializes the struct AND sets data_size. librime
  // gates every traits field behind RIME_STRUCT_HAS_MEMBER, which reads
  // data_size; without it the shared/user data dirs are silently ignored
  // and every deployment task fails (schema list not defined, no dict).
  RIME_STRUCT(RimeTraits, traits);
  traits.shared_data_dir = kSharedDataDir;
  traits.user_data_dir = kUserDataDir;
  traits.distribution_name = "xwx_ime";
  traits.distribution_code_name = "rime-oracle";
  traits.distribution_version = "0.1";
  traits.app_name = "rime.oracle";
  traits.modules = nullptr;
  traits.min_log_level = 2;  // ERROR and above only, keep stdout clean-ish
  traits.log_dir = "";       // log to stderr only, no files

  RimeSetup(&traits);
  RimeInitialize(&traits);

  // Canonical init sequence (mirrors tools/rime_api_console.cc): trigger a
  // full maintenance pass and block until it finishes. On a fresh .rime_user
  // this compiles luna_pinyin (tens of seconds); on later runs it is a cheap
  // no-op. Joining before creating any session guarantees the schema engine
  // and dictionary are ready, so a brand-new process never sees 0 candidates.
  if (RimeStartMaintenance(True)) {
    RimeJoinMaintenanceThread();
  }
}

// Run one input through a fresh session and print its record.
void ProcessInput(const std::string& input) {
  std::cout << "IN\t" << input << "\n";

  RimeSessionId session = RimeCreateSession();
  if (session == 0) {
    // Still honor the protocol so the peer can align records.
    std::cout << "PREEDIT\t\nNCAND\t0\nEND\n";
    return;
  }
  if (!RimeSelectSchema(session, kSchemaId)) {
    std::fprintf(stderr, "FATAL: RimeSelectSchema(%s) failed\n", kSchemaId);
    RimeDestroySession(session);
    std::cout << "PREEDIT\t\nNCAND\t0\nEND\n";
    return;
  }

  std::string preedit;
  long total_candidates = 0;
  std::string first_candidates[kMaxCandidatesToPrint];
  std::size_t kept = 0;

  if (RimeSimulateKeySequence(session, input.c_str())) {
    RimeContext context;
    RIME_STRUCT_INIT(RimeContext, context);
    if (RimeGetContext(session, &context)) {
      if (context.composition.preedit != nullptr) {
        preedit = context.composition.preedit;
      }
      RimeFreeContext(&context);
    }

    RimeCandidateListIterator iter;
    std::memset(&iter, 0, sizeof(iter));
    if (RimeCandidateListBegin(session, &iter)) {
      while (RimeCandidateListNext(&iter)) {
        ++total_candidates;
        if (kept < kMaxCandidatesToPrint && iter.candidate.text != nullptr) {
          first_candidates[kept++] = iter.candidate.text;
        }
      }
      RimeCandidateListEnd(&iter);
    }
  }

  RimeDestroySession(session);

  std::cout << "PREEDIT\t" << preedit << "\n";
  std::cout << "NCAND\t" << total_candidates << "\n";
  for (std::size_t i = 0; i < kept; ++i) {
    std::cout << "CAND\t" << (i + 1) << "\t" << first_candidates[i] << "\n";
  }
  std::cout << "END\n";
}

}  // namespace

int main() {
  InitRime();

  std::string line;
  while (std::getline(std::cin, line)) {
    if (!line.empty() && line.back() == '\r') {
      line.pop_back();
    }
    if (line.empty()) {
      continue;
    }
    if (line[0] == '#') {
      continue;
    }
    ProcessInput(line);
  }

  std::cout.flush();
  RimeFinalize();
  return 0;
}
