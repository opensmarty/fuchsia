// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef GARNET_LIB_UI_SCENIC_ENGINE_SESSION_HANDLER_H_
#define GARNET_LIB_UI_SCENIC_ENGINE_SESSION_HANDLER_H_

#include "lib/fidl/cpp/bindings/binding_set.h"
#include "lib/fidl/cpp/bindings/interface_ptr_set.h"
#include "lib/fxl/tasks/task_runner.h"

#include "garnet/lib/ui/mozart/util/error_reporter.h"
#include "garnet/lib/ui/scenic/engine/engine.h"
#include "garnet/lib/ui/scenic/engine/event_reporter.h"
#include "garnet/lib/ui/scenic/engine/session.h"
#include "lib/ui/scenic/fidl/session.fidl.h"

namespace scene_manager {

class SceneManagerImpl;

// Implements the Session FIDL interface.  For now, does nothing but buffer
// operations from Enqueue() before passing them all to |session_| when
// Commit() is called.  Eventually, this class may do more work if performance
// profiling suggests to.
class SessionHandler : public scenic::Session,
                       public EventReporter,
                       private mz::ErrorReporter {
 public:
  SessionHandler(Engine* engine,
                 SessionId session_id,
                 ::f1dl::InterfaceRequest<scenic::Session> request,
                 ::f1dl::InterfaceHandle<scenic::SessionListener> listener);
  ~SessionHandler() override;

  scene_manager::Session* session() const { return session_.get(); }

  // Flushes enqueued session events to the session listener as a batch.
  void SendEvents(::f1dl::Array<scenic::EventPtr> events) override;

 protected:
  // scenic::Session interface methods.
  void Enqueue(::f1dl::Array<scenic::OpPtr> ops) override;
  void Present(uint64_t presentation_time,
               ::f1dl::Array<zx::event> acquire_fences,
               ::f1dl::Array<zx::event> release_fences,
               const PresentCallback& callback) override;

  void HitTest(uint32_t node_id,
               scenic::vec3Ptr ray_origin,
               scenic::vec3Ptr ray_direction,
               const HitTestCallback& callback) override;

  void HitTestDeviceRay(
      scenic::vec3Ptr ray_origin,
      scenic::vec3Ptr ray_direction,
      const scenic::Session::HitTestCallback& callback) override;

 private:
  friend class Engine;

  // Customize behavior of mz::ErrorReporter::ReportError().
  void ReportError(fxl::LogSeverity severity,
                   std::string error_string) override;

  // Called by |binding_| when the connection closes. Must be invoked within
  // the SessionHandler MessageLoop.
  void BeginTearDown();

  // Called only by Engine. Use BeginTearDown() instead when you need to
  // teardown from within SessionHandler.
  void TearDown();

  Engine* const engine_;
  scene_manager::SessionPtr session_;

  ::f1dl::BindingSet<scenic::Session> bindings_;
  ::f1dl::InterfacePtr<scenic::SessionListener> listener_;

  ::f1dl::Array<scenic::OpPtr> buffered_ops_;
};

}  // namespace scene_manager

#endif  // GARNET_LIB_UI_SCENIC_ENGINE_SESSION_HANDLER_H_
