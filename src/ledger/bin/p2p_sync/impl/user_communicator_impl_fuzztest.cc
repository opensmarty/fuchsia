// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fit/function.h>
#include <lib/sys/cpp/component_context.h>

#include <algorithm>
#include <string>

#include "peridot/lib/convert/convert.h"
#include "src/ledger/bin/environment/environment.h"
#include "src/ledger/bin/p2p_provider/impl/p2p_provider_impl.h"
#include "src/ledger/bin/p2p_provider/public/p2p_provider.h"
#include "src/ledger/bin/p2p_sync/impl/user_communicator_impl.h"
#include "src/ledger/bin/storage/public/page_sync_client.h"
#include "src/ledger/bin/storage/testing/page_storage_empty_impl.h"
#include "src/lib/fxl/macros.h"

namespace p2p_sync {
namespace {
p2p_provider::P2PClientId MakeP2PClientId(uint8_t id) { return p2p_provider::P2PClientId({id}); }

class TestPageStorage : public storage::PageStorageEmptyImpl {
 public:
  TestPageStorage() = default;
  ~TestPageStorage() = default;

  storage::PageId GetId() override { return "page"; }

  void SetSyncDelegate(storage::PageSyncDelegate* page_sync) override { return; }
};

class FuzzingP2PProvider : public p2p_provider::P2PProvider {
 public:
  FuzzingP2PProvider() = default;

  void Start(Client* client) override { client_ = client; }

  bool SendMessage(const p2p_provider::P2PClientId& client_id,
                   convert::ExtendedStringView data) override {
    FXL_NOTIMPLEMENTED();
    return false;
  }

  Client* client_;
};

// Fuzz the peer-to-peer messages received by a |UserCommunicatorImpl|.
extern "C" int LLVMFuzzerTestOneInput(const uint8_t* Data, size_t Size) {
  std::string bytes(reinterpret_cast<const char*>(Data), Size);

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  auto component_context = sys::ComponentContext::Create();
  ledger::Environment environment(ledger::EnvironmentBuilder()
                                      .SetDisableStatistics(true)
                                      .SetAsync(loop.dispatcher())
                                      .SetIOAsync(loop.dispatcher())
                                      .SetStartupContext(component_context.get())
                                      .Build());
  auto provider = std::make_unique<FuzzingP2PProvider>();
  FuzzingP2PProvider* provider_ptr = provider.get();

  UserCommunicatorImpl user_communicator(&environment, std::move(provider));
  user_communicator.Start();
  auto ledger_communicator = user_communicator.GetLedgerCommunicator("ledger");

  storage::PageStorageEmptyImpl page_storage;

  auto page_communicator = ledger_communicator->GetPageCommunicator(&page_storage, &page_storage);

  provider_ptr->client_->OnDeviceChange(MakeP2PClientId(0), p2p_provider::DeviceChangeType::NEW);
  provider_ptr->client_->OnNewMessage(MakeP2PClientId(0), bytes);

  loop.RunUntilIdle();

  return 0;
}

}  // namespace
}  // namespace p2p_sync
