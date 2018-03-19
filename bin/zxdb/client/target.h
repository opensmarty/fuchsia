// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#pragma once

#include <functional>
#include <map>
#include <string>
#include <vector>

#include "garnet/bin/zxdb/client/client_object.h"
#include "garnet/bin/zxdb/client/weak_thunk.h"
#include "garnet/lib/debug_ipc/protocol.h"
#include "garnet/public/lib/fxl/observer_list.h"
#include "garnet/public/lib/fxl/macros.h"

namespace zxdb {

class Err;
class Process;
class System;
class TargetObserver;

// A Target represents the abstract idea of a process that can be debugged.
// This is as opposed to a Process which corresponds to one running process.
//
// Generally upon startup there would be a Target but no Process. This Target
// would receive the breakpoints, process name, command line switches, and
// other state from the user. Running this target would create the associated
// Process object. When the process exits, the Target can be re-used to
// launch the process again with the same configuration.
class Target : public ClientObject {
 public:
  using Callback = std::function<void(Target*, const Err&)>;


  enum State {
    // The process has not been started or has stopped. From here, it can only
    // transition to starting.
    kStopped,

    // A pending state when the process has been requested to be started but
    // there is no reply from the debug agent yet. From here, it can transition
    // to running (success) or stopped (if launching or attaching failed).
    kStarting,

    // The process is running. From here, it can only transition to stopped.
    kRunning
  };

  ~Target() override;

  void AddObserver(TargetObserver* observer);
  void RemoveObserver(TargetObserver* observer);

  // Returns the current process state.
  virtual State GetState() const = 0;

  // Returns the process object if it is currently running (see GetState()).
  // Returns null otherwise.
  virtual Process* GetProcess() const = 0;

  // Sets and retrieves the arguments passed to the program. args[0] is the
  // program name, the rest of the array are the command-line.
  virtual const std::vector<std::string>& GetArgs() const = 0;
  virtual void SetArgs(std::vector<std::string> args) = 0;

  // Returns the return code from the last time the process exited. If a
  // process has not yet been run and exited, this will be 0.
  virtual int64_t GetLastReturnCode() const = 0;

  // Launches the program. The program must be in a kStopped state and the
  // program name configured via SetArgs().
  virtual void Launch(Callback callback) = 0;

  // Attaches to the process with the given koid. The callback will be
  // executed with the attach is complete (or fails).
  virtual void Attach(uint64_t koid, Callback callback) = 0;

  // Detaches from the process with the given koid. The callback will be
  // executed with the detach is complete (or fails).
  virtual void Detach(Callback callback) = 0;

  // Notification from the agent that a process has exited.
  virtual void OnProcessExiting(int return_code) = 0;

 protected:
  explicit Target(Session* session);

  fxl::ObserverList<TargetObserver>& observers() { return observers_; }

 private:
  fxl::ObserverList<TargetObserver> observers_;

  FXL_DISALLOW_COPY_AND_ASSIGN(Target);
};

}  // namespace zxdb
