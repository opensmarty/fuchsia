// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#pragma once

#include <stdio.h>
#include <zircon/compiler.h>

#include "inc/kprivate/fsdriver.h"
#include "inc/posix.h"

// Symbol Definitions.

// Default block read limits to avoid read-disturb errors.
#define MLC_NAND_RC_LIMIT 100000
#define SLC_NAND_RC_LIMIT 1000000

// FsErrCode Error Code Assignments.
enum FsErrorCode {
  NDM_OK = 0,  // No errors.

  // TargetNDM Symbols.
  NDM_EIO = 1,             // Fatal I/O error.
  NDM_CFG_ERR = 2,         // NDM config error
  NDM_ASSERT = 3,          // Inconsistent NDM internal values.
  NDM_ENOMEM = 4,          // NDM memory allocation failure.
  NDM_SEM_CRE_ERR = 5,     // NDM semCreate() failed.
  NDM_NO_META_BLK = 6,     // No metadata block found.
  NDM_NO_META_DATA = 7,    // metadata page missing.
  NDM_BAD_META_DATA = 8,   // Invalid metadata contents.
  NDM_TOO_MANY_IBAD = 9,   // Too many initial bad blocks.
  NDM_TOO_MANY_RBAD = 10,  // Too many running bad blocks.
  NDM_NO_FREE_BLK = 11,    // No free block in NDM pool.
  NDM_IMAGE_RBB_CNT = 12,  // Bad block count in NDM image.
  NDM_RD_ECC_FAIL = 13,    // Read_page ECC decode failed.
  NDM_NOT_FOUND = 14,      // ndmDelDev() unknown handle.
  NDM_BAD_BLK_RECOV = 15,  // Running bad block recovery needed during RO-init.
  NDM_META_WR_REQ = 16,    // Metadata write request during RO-init.
  NDM_RBAD_LOCATION = 17,  // Running bad block replacement in virtual location.

  // TargetFTL-NDM Symbols.
  FTL_CFG_ERR = 20,         // FTL config error.
  FTL_ASSERT = 21,          // Inconsistent FTL internal values.
  FTL_ENOMEM = 22,          // FTL memory allocation failure.
  FTL_MOUNTED = 23,         // mount()/unformat() on mounted FTL.
  FTL_UNMOUNTED = 24,       // unmount() on unmounted FTL.
  FTL_NOT_FOUND = 25,       // FtlNdmDelVol() unknown name.
  FTL_NO_FREE_BLK = 26,     // No free FTL block.
  FTL_NO_MAP_BLKS = 27,     // No map block found during RO-init.
  FTL_NO_RECYCLE_BLK = 28,  // Recycle block selection failed.
  FTL_RECYCLE_CNT = 29,     // Repeated recycles did not free blocks.

  // Following would result in block erase except for RO-init flag.
  FTL_VOL_BLK_XFR = 40,  // Found interrupted volume block resume.
  FTL_MAP_BLK_XFR = 41,  // Found interrupted map block resume.
  FTL_UNUSED_MBLK = 42,  // Found unused map block during RO-init.
  FTL_VBLK_RESUME = 43,  // Low free block count: would resume volume block.
  FTL_MBLK_RESUME = 44,  // Low free block count: would resume map block.
};

// Macro Definitions.
// Bitmap accessors.
#define BITMAP_ON(start, i) (*((ui8*)(start) + (i) / 8) |= (ui8)(1 << ((i) % 8)))
#define BITMAP_OFF(start, i) (*((ui8*)(start) + (i) / 8) &= (ui8) ~(1 << ((i) % 8)))
#define IS_BITMAP_ON(start, i) (*((ui8*)(start) + (i) / 8) & (1 << ((i) % 8)))

// Type Definitions.

// FTL Wear Data Structure
typedef struct {
  double lag_sd;              // Standard deviation of block wear lag.
  double used_sd;             // Standard deviation of used pages per block.
  uint32_t recycle_cnt;       // Total number of recycles performed.
  uint32_t max_consec_rec;    // Maximum number of consecutive recycles.
  uint32_t avg_consec_rec;    // Average number of consecutive recycles.
  uint32_t wc_sum_recycles;   // # recycles when average lag exceeds limit.
  uint32_t wc_lim1_recycles;  // # recycles when a lag exceeds WC_LAG_LIM1.
  uint32_t wc_lim2_recycles;  // # recycles when a lag exceeds WC_LAG_LIM2.
  uint32_t write_amp_max;     // Max fl pgs per vol pgs in FtlnWrPages().
  uint32_t write_amp_avg;     // 10 x flash wr pgs per FtlnWrPages() pgs.
  uint32_t avg_wc_lag;        // Average wear count lag.
  uint32_t lag_ge_lim0;       // # of blks w/wear count lag >= lag limit 0.
  uint32_t lag_ge_lim1;       // # of blks w/wear count lag >= lag limit 1.
  uint32_t max_ge_lim2;       // Max blks w/wear lag concurrently >= lim2.
  uint32_t max_wc_over;       // # of times max delta (0xFF) was exceeded.
  uint8_t lft_max_lag;        // Lifetime max wear lag below hi wear count.
  uint8_t cur_max_lag;        // Current max wear lag.
} FtlWearData;

__BEGIN_CDECLS

// Function Prototypes.

int fsPerror(int fs_err_code);
int FsError(int err_code);
int FsError2(int fs_err_code, int errno_code);
int GetFsErrCode(void);
void SetFsErrCode(int error);
void* FtlnAddVol(FtlNdmVol* ftl_cfg, XfsVol* ftl_inf);
void FsMemPrn(void);
ui32 FsMemPeakRst(void);

void* FsCalloc(size_t nmemb, size_t size);
void* FsMalloc(size_t size);
void* FsAalloc(size_t size);
void FsFreeClear(void* ptr_ptr);
void FsAfreeClear(void* ptr_ptr);
void FsFree(void* ptr);

FtlWearData FtlnGetWearDat(void* ftl);
void FtlnSetLim0(uint32_t wc_lim0_lag, uint32_t wc_lim0_def);
void FtlnSetLim1(uint32_t wc_lim1_lag, uint32_t wc_lim1_def);
void FtlnSetLim2(uint32_t wc_lim2_lag);

__END_CDECLS
