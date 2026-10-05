/**
 * 批量添加扫描目录的共享逻辑（设置页与首页 onboarding 共用）。
 *
 * # 为什么不各写一份
 * 两处都要处理同一组边界：部分失败（多选里混一个不存在的路径）、
 * 成功后的视图更新、toast 文案。写两份必然漂移——
 * 典型症状是"设置页多选正常，首页多选丢目录"。
 *
 * # 🔴 逐个提交而非 Promise.all 快速失败
 * 快速失败会让"3 选 1 错"变成"3 个都没加上"，用户得重选一遍。
 * 逐个提交 + 失败清单回传，是部分成功场景下操作次数最少的方案。
 */

import { useCallback } from "react";
import { addScanDir } from "@/api/endpoints";
import type { ScanView } from "@/api/types";

export interface DirPickFailure {
  path: string;
  reason: string;
}

export interface UseAddDirsOptions {
  /** 至少成功一个时回调（传最新 ScanView），用于就地更新列表 */
  onChanged: (scan: ScanView) => void;
  onSuccess: (msg: string) => void;
}

/**
 * 返回 `pick(paths)`：提交一批目录，返回失败清单（空 = 全部成功）。
 * 调用方（弹窗）据此决定关闭还是保留失败项勾选供重试。
 */
export function useAddDirs({ onChanged, onSuccess }: UseAddDirsOptions) {
  return useCallback(
    async (paths: string[]): Promise<{ failed: DirPickFailure[] }> => {
      const failed: DirPickFailure[] = [];
      let added = 0;
      let lastView: ScanView | null = null;
      for (const p of paths) {
        try {
          lastView = await addScanDir(p);
          added += 1;
        } catch (err) {
          failed.push({ path: p, reason: err instanceof Error ? err.message : "添加失败" });
        }
      }
      if (lastView !== null) onChanged(lastView);
      if (added > 0) {
        onSuccess(
          failed.length === 0
            ? `已添加 ${added} 个目录`
            : `已添加 ${added} 个目录，${failed.length} 个失败（见弹窗提示）`,
        );
      }
      return { failed };
    },
    [onChanged, onSuccess],
  );
}
