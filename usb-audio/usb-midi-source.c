// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <ddk/device.h>
#include <driver/usb.h>
#include <zircon/device/midi.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <threads.h>

#include "midi.h"
#include "usb-audio.h"

#define READ_REQ_COUNT 20

typedef struct {
    zx_device_t* mxdev;
    zx_device_t* usb_mxdev;
    usb_protocol_t usb;

    // pool of free USB requests
    list_node_t free_read_reqs;
    // list of received packets not yet read by upper layer
    list_node_t completed_reads;
    // mutex for synchronizing access to free_read_reqs, completed_reads and open
    mtx_t mutex;

    bool open;
    bool dead;

    // the last signals we reported
    zx_signals_t signals;
} usb_midi_source_t;

static void update_signals(usb_midi_source_t* source) {
    zx_signals_t new_signals = 0;
    if (source->dead) {
        new_signals |= (DEV_STATE_READABLE | DEV_STATE_ERROR);
    } else if (!list_is_empty(&source->completed_reads)) {
        new_signals |= DEV_STATE_READABLE;
    }
    if (new_signals != source->signals) {
        device_state_clr_set(source->mxdev,
                             source->signals & ~new_signals,
                             new_signals & ~source->signals);
        source->signals = new_signals;
    }
}

static void usb_midi_source_read_complete(iotxn_t* txn, void* cookie) {
    usb_midi_source_t* source = (usb_midi_source_t*)cookie;

    if (txn->status == ZX_ERR_IO_NOT_PRESENT) {
        iotxn_release(txn);
        return;
    }

    mtx_lock(&source->mutex);

    if (txn->status == ZX_OK && txn->actual > 0) {
        list_add_tail(&source->completed_reads, &txn->node);
    } else {
        iotxn_queue(source->usb_mxdev, txn);
    }
    update_signals(source);
    mtx_unlock(&source->mutex);
}

static void usb_midi_source_unbind(void* ctx) {
    usb_midi_source_t* source = ctx;
    source->dead = true;
    update_signals(source);
    device_remove(source->mxdev);
}

static void usb_midi_source_free(usb_midi_source_t* source) {
    iotxn_t* txn;
    while ((txn = list_remove_head_type(&source->free_read_reqs, iotxn_t, node)) != NULL) {
        iotxn_release(txn);
    }
    while ((txn = list_remove_head_type(&source->completed_reads, iotxn_t, node)) != NULL) {
        iotxn_release(txn);
    }
    free(source);
}

static void usb_midi_source_release(void* ctx) {
    usb_midi_source_t* source = ctx;
    usb_midi_source_free(source);
}

static zx_status_t usb_midi_source_open(void* ctx, zx_device_t** dev_out, uint32_t flags) {
    usb_midi_source_t* source = ctx;
    zx_status_t result;

    mtx_lock(&source->mutex);
    if (source->open) {
        result = ZX_ERR_ALREADY_BOUND;
    } else {
        source->open = true;
        result = ZX_OK;
    }

    // queue up reads, including stale completed reads
    iotxn_t* txn;
    while ((txn = list_remove_head_type(&source->completed_reads, iotxn_t, node)) != NULL) {
        iotxn_queue(source->usb_mxdev, txn);
    }
    while ((txn = list_remove_head_type(&source->free_read_reqs, iotxn_t, node)) != NULL) {
        iotxn_queue(source->usb_mxdev, txn);
    }
    mtx_unlock(&source->mutex);

    return result;
}

static zx_status_t usb_midi_source_close(void* ctx, uint32_t flags) {
    usb_midi_source_t* source = ctx;

    mtx_lock(&source->mutex);
    source->open = false;
    mtx_unlock(&source->mutex);

    return ZX_OK;
}

static zx_status_t usb_midi_source_read(void* ctx, void* data, size_t len, zx_off_t off,
                                        size_t* actual) {
    usb_midi_source_t* source = ctx;

    if (source->dead) {
        return ZX_ERR_IO_NOT_PRESENT;
    }

    zx_status_t status = ZX_OK;
    if (len < 3) return ZX_ERR_BUFFER_TOO_SMALL;

    mtx_lock(&source->mutex);

    list_node_t* node = list_peek_head(&source->completed_reads);
    if (!node) {
        status = ZX_ERR_SHOULD_WAIT;
        goto out;
    }
    iotxn_t* txn = containerof(node, iotxn_t, node);

    // MIDI events are 4 bytes. We can ignore the zeroth byte
    iotxn_copyfrom(txn, data, 3, 1);
    *actual = get_midi_message_length(*((uint8_t *)data));
    list_remove_head(&source->completed_reads);
    list_add_head(&source->free_read_reqs, &txn->node);
    while ((node = list_remove_head(&source->free_read_reqs)) != NULL) {
        iotxn_t* req = containerof(node, iotxn_t, node);
        iotxn_queue(source->usb_mxdev, req);
    }

out:
    update_signals(source);
    mtx_unlock(&source->mutex);
    return status;
}

static zx_status_t usb_midi_source_ioctl(void* ctx, uint32_t op, const void* in_buf,
                                         size_t in_len, void* out_buf, size_t out_len,
                                         size_t* out_actual) {
    switch (op) {
    case IOCTL_MIDI_GET_DEVICE_TYPE: {
        int* reply = out_buf;
        if (out_len < sizeof(*reply)) return ZX_ERR_BUFFER_TOO_SMALL;
        *reply = MIDI_TYPE_SOURCE;
        *out_actual = sizeof(*reply);
        return ZX_OK;
    }
    }

    return ZX_ERR_NOT_SUPPORTED;
}

static zx_protocol_device_t usb_midi_source_device_proto = {
    .version = DEVICE_OPS_VERSION,
    .unbind = usb_midi_source_unbind,
    .release = usb_midi_source_release,
    .open = usb_midi_source_open,
    .close = usb_midi_source_close,
    .read = usb_midi_source_read,
    .ioctl = usb_midi_source_ioctl,
};

zx_status_t usb_midi_source_create(zx_device_t* device, usb_protocol_t* usb, int index,
                                  usb_interface_descriptor_t* intf, usb_endpoint_descriptor_t* ep) {
    usb_midi_source_t* source = calloc(1, sizeof(usb_midi_source_t));
    if (!source) {
        printf("Not enough memory for usb_midi_source_t\n");
        return ZX_ERR_NO_MEMORY;
    }

    list_initialize(&source->free_read_reqs);
    list_initialize(&source->completed_reads);
    source->usb_mxdev = device;
    memcpy(&source->usb, usb, sizeof(source->usb));

    int packet_size = usb_ep_max_packet(ep);
    if (intf->bAlternateSetting != 0) {
        usb_set_interface(usb, intf->bInterfaceNumber, intf->bAlternateSetting);
    }
    for (int i = 0; i < READ_REQ_COUNT; i++) {
        iotxn_t* txn = usb_alloc_iotxn(ep->bEndpointAddress, packet_size);
        if (!txn) {
            usb_midi_source_free(source);
            return ZX_ERR_NO_MEMORY;
        }
        txn->length = packet_size;
        txn->complete_cb = usb_midi_source_read_complete;
        txn->cookie = source;
        list_add_head(&source->free_read_reqs, &txn->node);
    }

    char name[ZX_DEVICE_NAME_MAX];
    snprintf(name, sizeof(name), "usb-midi-source-%d\n", index);

    device_add_args_t args = {
        .version = DEVICE_ADD_ARGS_VERSION,
        .name = name,
        .ctx = source,
        .ops = &usb_midi_source_device_proto,
        .proto_id = ZX_PROTOCOL_MIDI,
    };

    zx_status_t status = device_add(device, &args, &source->mxdev);
    if (status != ZX_OK) {
        printf("device_add failed in usb_midi_source_create\n");
        usb_midi_source_free(source);
    }

    return status;
}
