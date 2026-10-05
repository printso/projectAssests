/**
 * 设置页：大模型配置 / 扫描目录 / 外观 / 数据与隐私。
 *
 * # 🔴 API Key 掩码协议（安全关键）
 * 后端**永不**返回明文 key，只返回掩码串（`cloud_api_key_masked`，如 `sk-s…890`）
 * 与一个哨兵值（`api_key_placeholder` = `"__unchanged__"`）。
 *
 * 前端规则：
 * - 输入框初始为**空**，placeholder 显示掩码串（让用户知道"已配置过一个 key"）
 * - 用户没动这个框 → 提交时发 `api_key_placeholder`（哨兵），后端识别为"保留原 key"
 * - 用户输入了新内容 → 提交用户输入
 *
 * 🔴 **绝不能把掩码串当明文回传**：那会把 `sk-s…890` 写进配置，
 * 之后所有连接测试永远失败，而且用户看不出问题在哪（他以为自己没动过 key）。
 * 这是后端注释里明确记录过的原型期真实缺陷。
 *
 * # 🔴 Local-First 红线在 UI 上必须可见
 * `sensitive_local_only` / `embedding_local_only` 决定用户的代码内容会不会离开本机。
 * 这两个开关的说明必须讲清后果，不能只写个标签。
 *
 * # 🔴 测试连接的结果要能帮用户纠错
 * `testConnection` 返回后端实际探测到的 `models` 列表。
 * 用户填错模型名时，用它给出"你是不是想用 xxx"的引导，
 * 比一句"连接失败"有用得多。
 *
 * # 🔴 清理派生数据是破坏性操作
 * 走确认弹窗，且把 `cleared`（各表清理行数）与 `preserved`（刻意保留的）都展示出来。
 */

import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  addScanDir,
  clearDerived,
  exportSettings,
  getAuditLog,
  getSettings,
  removeScanDir,
  testConnection,
  toggleScanDir,
  updateAppearance,
  updateLlmSettings,
  updateScanSettings,
  type SettingsUpdatePayload,
} from "@/api/endpoints";
import type { AppearanceSettings, AuditView, ClearResult, LlmView, SettingsView, TestConnectionView } from "@/api/types";
import { useAsync } from "@/lib/useAsync";
import { useAddDirs } from "@/lib/useAddDirs";
import { useToast } from "@/components/Toast";
import { ConfirmModal } from "@/components/ConfirmModal";
import { DirPickerModal } from "@/components/DirPickerModal";
import { Button, Card, InlineEmpty, KV, PageHead, Tag } from "@/components/ui";
import { ErrorStateWithNav, Loading } from "@/components/States";
import { Icon } from "@/components/Icon";
import { formatBytes } from "@/components/Sidebar";
import { timeAgo } from "@/lib/format";

export interface SettingsPageProps {
  /** 外观保存后同步到全局（让主题立刻生效，无需刷新） */
  onAppearanceChange: (appearance: AppearanceSettings) => void;
}

export function SettingsPage({ onAppearanceChange }: SettingsPageProps) {
  const navigate = useNavigate();
  const toast = useToast();
  const { data, error, loading, reload, mutate } = useAsync<SettingsView>((signal) => getSettings(signal), []);

  if (error !== null) {
    return <ErrorStateWithNav error={error} onRetry={reload} navigate={navigate} />;
  }
  if (loading && data === null) {
    return <Loading rows={6} label="加载设置" />;
  }
  if (data === null) {
    return <InlineEmpty>设置为空。</InlineEmpty>;
  }

  return (
    <>
      <PageHead
        title="设置"
        sub="单机版：所有配置只存本机，无账号体系。大模型配置决定分析能力，扫描目录决定数据来源。"
        actions={
          <Button size="sm" icon="refresh" onClick={reload} busy={loading}>
            重新加载
          </Button>
        }
      />

      <LlmSection
        view={data}
        // 🔴 该端点返回扁平的 LlmView，只能合并进 data.llm；
        // 曾用它整体覆盖 SettingsView，导致保存后其余段落变 undefined、页面白屏
        onSaved={(next) => mutate((prev) => ({ ...prev, llm: next }))}
        onError={(msg) => toast.error(msg)}
        onSuccess={(msg) => toast.success(msg)}
      />

      <ScanDirsSection
        view={data}
        onChanged={(scan) => mutate((prev) => ({ ...prev, scan }))}
        onError={(msg) => toast.error(msg)}
        onSuccess={(msg) => toast.success(msg)}
      />

      <AppearanceSection
        appearance={data.appearance}
        onSaved={(next) => {
          mutate((prev) => ({ ...prev, appearance: next }));
          onAppearanceChange(next);
        }}
        onError={(msg) => toast.error(msg)}
      />

      <DataSection
        view={data}
        onCleared={() => {
          reload();
          toast.success("派生数据已清理，可重新扫描生成");
        }}
        onError={(msg) => toast.error(msg)}
      />
    </>
  );
}

// ══════════════════════════════════════════════════════════════════
// 大模型配置
// ══════════════════════════════════════════════════════════════════

interface LlmSectionProps {
  view: SettingsView;
  /** 保存成功后回传后端最新的 LlmView（扁平结构，由父组件合并进 data.llm） */
  onSaved: (next: LlmView) => void;
  onError: (msg: string) => void;
  onSuccess: (msg: string) => void;
}

function LlmSection({ view, onSaved, onError, onSuccess }: LlmSectionProps) {
  const llm = view.llm;

  // 表单本地态：与后端值分离，未保存的修改不会污染 data
  const [cloudProvider, setCloudProvider] = useState(llm.cloud_provider);
  const [cloudBaseUrl, setCloudBaseUrl] = useState(llm.cloud_base_url);
  // 🔴 API key 输入框初始为空（不是掩码串）：见文件头的掩码协议
  const [cloudApiKey, setCloudApiKey] = useState("");
  const [cloudModel, setCloudModel] = useState(llm.cloud_model);
  const [localBackend, setLocalBackend] = useState(llm.local_backend);
  const [localBaseUrl, setLocalBaseUrl] = useState(llm.local_base_url);
  const [localModel, setLocalModel] = useState(llm.local_model);
  const [routeFast, setRouteFast] = useState(llm.route_fast);
  const [routeDeep, setRouteDeep] = useState(llm.route_deep);
  const [sensitiveLocalOnly, setSensitiveLocalOnly] = useState(llm.sensitive_local_only);
  const [embeddingLocalOnly, setEmbeddingLocalOnly] = useState(llm.embedding_local_only);
  const [showKey, setShowKey] = useState(false);

  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [enablingCloud, setEnablingCloud] = useState(false);
  const [testResult, setTestResult] = useState<TestConnectionView | null>(null);

  // 后端值变化时（例如别处保存过）同步表单
  useEffect(() => {
    setCloudProvider(llm.cloud_provider);
    setCloudBaseUrl(llm.cloud_base_url);
    setCloudModel(llm.cloud_model);
    setLocalBackend(llm.local_backend);
    setLocalBaseUrl(llm.local_base_url);
    setLocalModel(llm.local_model);
    setRouteFast(llm.route_fast);
    setRouteDeep(llm.route_deep);
    setSensitiveLocalOnly(llm.sensitive_local_only);
    setEmbeddingLocalOnly(llm.embedding_local_only);
    // 🔴 刻意不同步 cloudApiKey：它必须保持"空 = 未修改"的语义
  }, [llm]);

  const handleSave = useCallback(async () => {
    setSaving(true);
    try {
      const payload: NonNullable<SettingsUpdatePayload["llm"]> = {
        cloud_provider: cloudProvider,
        cloud_base_url: cloudBaseUrl,
        // 🔴 掩码协议：空 = 用户没改 → 发哨兵值让后端保留原 key
        cloud_api_key: cloudApiKey.trim() === "" ? llm.api_key_placeholder : cloudApiKey.trim(),
        cloud_model: cloudModel,
        local_backend: localBackend,
        local_base_url: localBaseUrl,
        local_model: localModel,
        route_fast: routeFast,
        route_deep: routeDeep,
        sensitive_local_only: sensitiveLocalOnly,
        embedding_local_only: embeddingLocalOnly,
      };
      const next = await updateLlmSettings(payload);
      onSaved(next);
      setCloudApiKey(""); // 保存成功后清空，恢复"未修改"态
      setTestResult(null);
      onSuccess("大模型配置已保存");
    } catch (err) {
      const msg = err instanceof Error ? err.message : "保存失败";
      const hint = err instanceof Error && "hint" in err ? String((err as { hint?: string }).hint ?? "") : "";
      onError(hint ? `${msg}（${hint}）` : msg);
    } finally {
      setSaving(false);
    }
  }, [
    cloudProvider, cloudBaseUrl, cloudApiKey, cloudModel, localBackend, localBaseUrl,
    localModel, routeFast, routeDeep, sensitiveLocalOnly, embeddingLocalOnly,
    llm.api_key_placeholder, onSaved, onError, onSuccess,
  ]);

  const handleTest = useCallback(async () => {
    setTesting(true);
    setTestResult(null);
    try {
      const r = await testConnection();
      setTestResult(r);
    } catch (err) {
      const msg = err instanceof Error ? err.message : "测试失败";
      const hint = err instanceof Error && "hint" in err ? String((err as { hint?: string }).hint ?? "") : "";
      onError(hint ? `${msg}（${hint}）` : msg);
    } finally {
      setTesting(false);
    }
  }, [onError]);

  /**
   * 一键把快速/深度路由切到云端并保存。
   *
   * 🔴 为什么需要它：用户配好了云端 key 却忘了改路由（默认 Local-First 全本地），
   * 结果"测试连接"永远报本地未配置、AI 功能看似"无法使用"，而界面上
   * 没有任何一处告诉他原因。这个按钮把"配好了但没启用"变成一步可达。
   * 只提交路由两个字段：其余配置（含 key 哨兵）保持后端现值不动。
   */
  const handleEnableCloudRoute = useCallback(async () => {
    setEnablingCloud(true);
    try {
      const next = await updateLlmSettings({ route_fast: "cloud", route_deep: "cloud" });
      onSaved(next);
      setTestResult(null);
      onSuccess("已启用云端路由，可点「测试连接」验证");
    } catch (err) {
      const msg = err instanceof Error ? err.message : "启用失败";
      const hint = err instanceof Error && "hint" in err ? String((err as { hint?: string }).hint ?? "") : "";
      onError(hint ? `${msg}（${hint}）` : msg);
    } finally {
      setEnablingCloud(false);
    }
  }, [onSaved, onError, onSuccess]);

  return (
    <Card
      icon="spark"
      title="大模型配置"
      sub="Local-First：默认优先本地模型，敏感项目强制只用本地。未配置时分析师走离线检索式回答。"
      style={{ marginBottom: 16 }}
      action={
        <span className={`badge ${llm.local_configured || llm.cloud_configured ? "badge--high" : "badge--muted"}`}>
          {llm.local_configured ? "本地已配置" : llm.cloud_configured ? "云端已配置" : "未配置"}
        </span>
      }
    >
      {/* ── 本地模型 ─────────────────────────────────────── */}
      <div className="card-title" style={{ fontSize: "var(--fs-md)", margin: "4px 0 10px" }}>
        <Icon name="db" /> 本地模型（推荐，代码不出本机）
      </div>
      <div className="form-row">
        <div className="form-label">后端</div>
        <div className="form-field">
          <select value={localBackend} onChange={(e) => {
            setLocalBackend(e.target.value);
            // 切换后端时带上它的默认地址，省得用户手填
            const opt = llm.local_backends.find((b) => b.value === e.target.value);
            if (opt !== undefined && localBaseUrl === "") setLocalBaseUrl(opt.default_base_url);
          }}>
            {llm.local_backends.map((b) => (
              <option key={b.value} value={b.value}>
                {b.label}
              </option>
            ))}
          </select>
        </div>
      </div>
      <div className="form-row">
        <div className="form-label">服务地址</div>
        <div className="form-field">
          <input
            className="mono"
            value={localBaseUrl}
            onChange={(e) => setLocalBaseUrl(e.target.value)}
            placeholder="http://127.0.0.1:11434"
          />
          <div className="form-hint">
            本地端点会绕过系统代理（避免 localhost 被代理劫持）。需先启动对应服务（如 <code>ollama serve</code>）。
          </div>
        </div>
      </div>
      <div className="form-row">
        <div className="form-label">模型名</div>
        <div className="form-field">
          <input
            className="mono"
            value={localModel}
            onChange={(e) => setLocalModel(e.target.value)}
            placeholder="qwen2.5:14b"
            list="local-model-presets"
          />
          <datalist id="local-model-presets">
            {llm.local_backends
              .find((b) => b.value === localBackend)
              ?.preset_models.map((m) => <option key={m} value={m} />)}
          </datalist>
        </div>
      </div>

      {/* ── 云端模型 ─────────────────────────────────────── */}
      <div className="card-title" style={{ fontSize: "var(--fs-md)", margin: "18px 0 10px" }}>
        <Icon name="cloud" /> 云端模型（能力更强，但代码会离开本机）
      </div>
      <div className="form-row">
        <div className="form-label">服务商</div>
        <div className="form-field">
          <select value={cloudProvider} onChange={(e) => {
            setCloudProvider(e.target.value);
            const opt = llm.cloud_providers.find((p) => p.value === e.target.value);
            if (opt !== undefined && cloudBaseUrl === "") setCloudBaseUrl(opt.default_base_url);
          }}>
            {llm.cloud_providers.map((p) => (
              <option key={p.value} value={p.value}>
                {p.label}
              </option>
            ))}
          </select>
        </div>
      </div>
      <div className="form-row">
        <div className="form-label">Base URL</div>
        <div className="form-field">
          <input className="mono" value={cloudBaseUrl} onChange={(e) => setCloudBaseUrl(e.target.value)} placeholder="https://api.openai.com/v1" />
        </div>
      </div>
      <div className="form-row">
        <div className="form-label">API Key</div>
        <div className="form-field">
          <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
            <input
              className="mono"
              style={{ flex: 1 }}
              type={showKey ? "text" : "password"}
              value={cloudApiKey}
              onChange={(e) => setCloudApiKey(e.target.value)}
              // 🔴 placeholder 显示掩码串：让用户知道"已经配过一个 key"，
              // 同时输入框实际为空 → 保存时发哨兵值，不会覆盖原 key
              placeholder={llm.cloud_configured ? `已配置：${llm.cloud_api_key_masked}` : "留空则不修改"}
              autoComplete="off"
            />
            <button
              type="button"
              className="icon-btn"
              onClick={() => setShowKey((v) => !v)}
              title={showKey ? "隐藏" : "显示"}
              aria-label={showKey ? "隐藏 API Key" : "显示 API Key"}
            >
              <Icon name={showKey ? "eyeoff" : "eye"} />
            </button>
          </div>
          <div className="form-hint">
            留空表示不修改现有 key。明文 key 只存在本机数据库，界面上永远只显示掩码。
          </div>
        </div>
      </div>
      <div className="form-row">
        <div className="form-label">模型名</div>
        <div className="form-field">
          <input className="mono" value={cloudModel} onChange={(e) => setCloudModel(e.target.value)} placeholder="gpt-4o-mini" list="cloud-model-presets" />
          <datalist id="cloud-model-presets">
            {llm.cloud_providers
              .find((p) => p.value === cloudProvider)
              ?.preset_models.map((m) => <option key={m} value={m} />)}
          </datalist>
        </div>
      </div>

      {/* ── 路由与安全 ───────────────────────────────────── */}
      <div className="card-title" style={{ fontSize: "var(--fs-md)", margin: "18px 0 10px" }}>
        <Icon name="shield" /> 路由与安全红线
      </div>
      {/* 🔴 "配好了但没启用"必须可见：用户配好云端却忘了改路由时，
          测试连接会一直报本地未配置、AI 看似无法使用，而原因无处可查。
          这里把状态摊开，并给一步可达的启用入口。 */}
      {llm.cloud_configured && routeFast === "local" && routeDeep === "local" ? (
        <div
          style={{
            display: "flex",
            gap: 10,
            alignItems: "center",
            flexWrap: "wrap",
            marginBottom: 12,
            padding: "10px 14px",
            borderRadius: "var(--r-md)",
            background: "color-mix(in srgb, var(--color-warning) 10%, transparent)",
            border: "1px solid color-mix(in srgb, var(--color-warning) 30%, transparent)",
            fontSize: "var(--fs-sm)",
          }}
        >
          <Icon name="alert" />
          <span style={{ flex: 1, minWidth: 200 }}>
            云端模型已配置，但快速/深度路由仍指向本地模型，云端不会被使用
            {llm.local_configured ? "" : "（且本地后端未配置，AI 功能将不可用）"}。
          </span>
          <Button size="sm" variant="primary" icon="cloud" busy={enablingCloud} onClick={() => void handleEnableCloudRoute()}>
            启用云端路由
          </Button>
        </div>
      ) : null}
      <div className="form-row">
        <div className="form-label">快速路由</div>
        <div className="form-field">
          <select value={routeFast} onChange={(e) => setRouteFast(e.target.value)}>
            <option value="local">本地模型</option>
            <option value="cloud">云端模型</option>
          </select>
          <div className="form-hint">用于画像、摘要等轻量任务。</div>
        </div>
      </div>
      <div className="form-row">
        <div className="form-label">深度路由</div>
        <div className="form-field">
          <select value={routeDeep} onChange={(e) => setRouteDeep(e.target.value)}>
            <option value="local">本地模型</option>
            <option value="cloud">云端模型</option>
          </select>
          <div className="form-hint">用于对话式分析师等需要推理的任务。</div>
        </div>
      </div>
      <div className="form-row">
        <div className="form-label">敏感项目仅本地</div>
        <div className="form-field">
          <label style={{ display: "flex", gap: 8, alignItems: "center", cursor: "pointer" }}>
            <input type="checkbox" checked={sensitiveLocalOnly} onChange={(e) => setSensitiveLocalOnly(e.target.checked)} />
            <span>标记为敏感的项目，绝不使用云端模型</span>
          </label>
          <div className="form-hint">
            🔴 这是 Local-First 的安全红线。开启后，敏感项目的代码内容永远不会发往云端服务商，
            即使深度路由配置为云端也会被强制降级到本地。关闭它意味着你接受敏感代码可能被上传。
          </div>
        </div>
      </div>
      <div className="form-row">
        <div className="form-label">向量生成仅本地</div>
        <div className="form-field">
          <label style={{ display: "flex", gap: 8, alignItems: "center", cursor: "pointer" }}>
            <input type="checkbox" checked={embeddingLocalOnly} onChange={(e) => setEmbeddingLocalOnly(e.target.checked)} />
            <span>Embedding 只在本地生成</span>
          </label>
          <div className="form-hint">代码片段会被送去生成向量。开启后这些片段不会离开本机。</div>
        </div>
      </div>

      {/* ── 动作区 ───────────────────────────────────────── */}
      <div style={{ display: "flex", gap: 8, marginTop: 16, flexWrap: "wrap", alignItems: "center" }}>
        <Button variant="primary" icon="check" busy={saving} onClick={() => void handleSave()}>
          保存配置
        </Button>
        <Button
          icon="plug"
          busy={testing}
          onClick={() => void handleTest()}
          disabled={!llm.local_configured && !llm.cloud_configured && cloudApiKey === ""}
          title={!llm.local_configured && !llm.cloud_configured ? "请先填写并保存模型配置" : undefined}
        >
          测试连接
        </Button>
      </div>

      {/* 🔴 测试结果：成功给延迟，失败给后端探测到的可用模型列表帮用户纠错 */}
      {testResult !== null ? (
        <div
          style={{
            marginTop: 12,
            padding: "10px 14px",
            borderRadius: "var(--r-md)",
            background: testResult.ok
              ? "color-mix(in srgb, var(--color-success) 10%, transparent)"
              : "color-mix(in srgb, var(--color-danger) 10%, transparent)",
            border: `1px solid ${testResult.ok ? "color-mix(in srgb, var(--color-success) 30%, transparent)" : "color-mix(in srgb, var(--color-danger) 30%, transparent)"}`,
            fontSize: "var(--fs-sm)",
          }}
        >
          <div>
            <Icon name={testResult.ok ? "check" : "alert"} /> {testResult.message}
          </div>
          {testResult.ok ? (
            <div style={{ color: "var(--color-text-3)", marginTop: 4 }}>
              {testResult.backend} · {testResult.model} · {testResult.route_label} · 延迟 {testResult.latency_ms}ms
            </div>
          ) : null}
          {testResult.models.length > 0 ? (
            <div style={{ marginTop: 8 }}>
              <div style={{ color: "var(--color-text-3)", marginBottom: 4 }}>
                {testResult.ok ? "该端点可用模型：" : "后端探测到这些模型，是否想用其中之一？"}
              </div>
              <div className="chips">
                {testResult.models.slice(0, 12).map((m) => (
                  <button
                    key={m}
                    type="button"
                    className="chip mono"
                    onClick={() => {
                      // 本地后端填 localModel，否则填 cloudModel
                      if (testResult.route.includes("local")) setLocalModel(m);
                      else setCloudModel(m);
                    }}
                    title="点击填入模型名"
                  >
                    {m}
                  </button>
                ))}
              </div>
            </div>
          ) : null}
        </div>
      ) : null}
    </Card>
  );
}

// ══════════════════════════════════════════════════════════════════
// 扫描目录
// ══════════════════════════════════════════════════════════════════

interface ScanDirsSectionProps {
  view: SettingsView;
  onChanged: (scan: SettingsView["scan"]) => void;
  onError: (msg: string) => void;
  onSuccess: (msg: string) => void;
}

function ScanDirsSection({ view, onChanged, onError, onSuccess }: ScanDirsSectionProps) {
  const scan = view.scan;
  const [newDir, setNewDir] = useState("");
  const [adding, setAdding] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);

  const handleAdd = useCallback(async () => {
    const path = newDir.trim();
    if (path === "") {
      onError("请输入目录路径");
      return;
    }
    setAdding(true);
    try {
      const next = await addScanDir(path);
      onChanged(next);
      setNewDir("");
      onSuccess(`已添加目录：${path}`);
    } catch (err) {
      // 🔴 后端会校验目录真实存在，不存在返回 400 + 可操作 hint
      const msg = err instanceof Error ? err.message : "添加失败";
      const hint = err instanceof Error && "hint" in err ? String((err as { hint?: string }).hint ?? "") : "";
      onError(hint ? `${msg}（${hint}）` : msg);
    } finally {
      setAdding(false);
    }
  }, [newDir, onChanged, onError, onSuccess]);

  /**
   * 弹窗批量添加：逻辑见 `lib/useAddDirs`（设置页与首页 onboarding 共用，
   * 避免两处对"部分失败"的处理漂移）。
   */
  const handlePick = useAddDirs({ onChanged, onSuccess });

  const handleToggle = useCallback(
    async (path: string, enabled: boolean) => {
      try {
        const next = await toggleScanDir(path, enabled);
        onChanged(next);
        onSuccess(enabled ? `已启用：${path}` : `已停用：${path}（保留配置，不再扫描）`);
      } catch (err) {
        onError(err instanceof Error ? err.message : "操作失败");
      }
    },
    [onChanged, onError, onSuccess],
  );

  const handleRemove = useCallback(
    async (path: string) => {
      try {
        const next = await removeScanDir(path);
        onChanged(next);
        onSuccess(`已移除目录：${path}`);
      } catch (err) {
        onError(err instanceof Error ? err.message : "移除失败");
      }
    },
    [onChanged, onError, onSuccess],
  );

  return (
    <Card
      icon="folder"
      title="扫描目录"
      sub="只有这里授权的目录才会被读取。停用的目录保留配置但不参与扫描。"
      style={{ marginBottom: 16 }}
    >
      {/* 🔴 配置问题（目录被移动/删除/无权限）必须显眼提示 */}
      {scan.problems.length > 0 ? (
        <div
          style={{
            marginBottom: 12,
            padding: "10px 14px",
            borderRadius: "var(--r-md)",
            background: "color-mix(in srgb, var(--color-warning) 10%, transparent)",
            border: "1px solid color-mix(in srgb, var(--color-warning) 30%, transparent)",
            fontSize: "var(--fs-sm)",
          }}
        >
          {scan.problems.map((p) => (
            <div key={p}>
              <Icon name="alert" /> {p}
            </div>
          ))}
        </div>
      ) : null}

      {scan.dirs.length === 0 ? (
        <InlineEmpty>
          还没有授权任何目录。点「选择目录…」勾选你的代码根目录（例如 <code>F:/CodeProject</code>），扫描器会递归发现其中的项目。
        </InlineEmpty>
      ) : (
        scan.dirs.map((d) => (
          <div className="dir-row" key={d.path}>
            <label style={{ display: "flex", gap: 8, alignItems: "center", flex: 1, minWidth: 0, cursor: "pointer" }}>
              <input
                type="checkbox"
                checked={d.enabled}
                onChange={(e) => void handleToggle(d.path, e.target.checked)}
                aria-label={`${d.enabled ? "停用" : "启用"} ${d.path}`}
              />
              <span className="mono" style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis" }} title={d.path}>
                {d.path}
              </span>
              {/* 🔴 目录已不存在时明确标出：否则用户会以为扫描器坏了 */}
              {!d.exists ? <span className="badge badge--pink">目录不存在</span> : null}
            </label>
            <span style={{ fontSize: "var(--fs-xs)", color: "var(--color-text-3)", whiteSpace: "nowrap" }}>
              {d.project_count !== null ? `${d.project_count} 个项目 · ` : ""}
              {d.last_scanned_at !== null ? `上次 ${timeAgo(d.last_scanned_at)}` : "尚未扫描"}
            </span>
            <button
              type="button"
              className="icon-btn"
              onClick={() => void handleRemove(d.path)}
              title="移除该目录"
              aria-label={`移除 ${d.path}`}
            >
              <Icon name="trash" />
            </button>
          </div>
        ))
      )}

      {/* 🔴 主入口是「选择目录」弹窗（点选符合用户习惯）；
          手输降级为次级入口——粘贴路径仍是有价值的兜底（如网络驱动器、
          弹窗列不出的深层目录），但不该是默认姿势。 */}
      <div style={{ display: "flex", gap: 8, marginTop: 14, alignItems: "center", flexWrap: "wrap" }}>
        <Button variant="primary" icon="folder" onClick={() => setPickerOpen(true)}>
          选择目录…
        </Button>
        <input
          className="mono"
          style={{ flex: 1, minWidth: 220 }}
          value={newDir}
          onChange={(e) => setNewDir(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void handleAdd();
          }}
          placeholder="或手动粘贴绝对路径，例如 F:/CodeProject"
          aria-label="新目录路径"
        />
        <Button icon="plus" busy={adding} onClick={() => void handleAdd()}>
          添加
        </Button>
      </div>

      {pickerOpen ? (
        <DirPickerModal onClose={() => setPickerOpen(false)} onPick={handlePick} />
      ) : null}

      {/* 扫描行为参数 */}
      <ScanOptions scan={scan} onChanged={onChanged} onError={onError} onSuccess={onSuccess} />
    </Card>
  );
}

function ScanOptions({
  scan,
  onChanged,
  onError,
  onSuccess,
}: {
  scan: SettingsView["scan"];
  onChanged: (scan: SettingsView["scan"]) => void;
  onError: (msg: string) => void;
  onSuccess: (msg: string) => void;
}) {
  const [maxDepth, setMaxDepth] = useState(scan.max_depth);
  const [watch, setWatch] = useState(scan.watch_enabled);
  const [level2, setLevel2] = useState(scan.level2_enabled);
  const [patterns, setPatterns] = useState(scan.exclude_patterns.join("\n"));

  const save = useCallback(async () => {
    try {
      const next = await updateScanSettings({
        max_depth: maxDepth,
        watch_enabled: watch,
        level2_enabled: level2,
        // 每行一个模式，忽略空行
        exclude_patterns: patterns
          .split("\n")
          .map((p) => p.trim())
          .filter((p) => p !== ""),
      });
      onChanged(next);
      onSuccess("扫描设置已保存");
    } catch (err) {
      onError(err instanceof Error ? err.message : "保存失败");
    }
  }, [maxDepth, watch, level2, patterns, onChanged, onError, onSuccess]);

  return (
    <div style={{ marginTop: 18, paddingTop: 14, borderTop: "1px solid var(--color-border)" }}>
      <div className="form-row">
        <div className="form-label">最大递归深度</div>
        <div className="form-field">
          <input
            type="number"
            min={1}
            max={32}
            value={maxDepth}
            onChange={(e) => setMaxDepth(Number.parseInt(e.target.value, 10) || 1)}
            style={{ width: 90 }}
          />
          <div className="form-hint">目录树向下探索的层数。过大会拖慢扫描并纳入无关的深层目录。</div>
        </div>
      </div>
      <div className="form-row">
        <div className="form-label">文件监听</div>
        <div className="form-field">
          <label style={{ display: "flex", gap: 8, alignItems: "center", cursor: "pointer" }}>
            <input type="checkbox" checked={watch} onChange={(e) => setWatch(e.target.checked)} />
            <span>监听变更并增量重扫</span>
          </label>
        </div>
      </div>
      {/* 🔴 这个开关的字段名是 `scan.level2_enabled`（后端把它放在 ScanSettings 里），
          但它实际管的是**是否允许调用模型**——与扫描本身无关。
          原文案「Level 2 分析 / 抽取资产与能力（耗时较长）」是错的：
          资产与能力抽取发生在 Level 1（IndexCodeHandler），关掉这个开关它们照样跑。
          文案必须按真实行为写，否则用户关掉后发现资产还在增加，
          会以为开关坏了，进而不再信任任何隐私开关。
          字段留在 scan 段而不迁移：迁移要动 schema 与三处端点，收益不抵成本。 */}
      <div className="form-row">
        <div className="form-label">AI 分析</div>
        <div className="form-field">
          <label style={{ display: "flex", gap: 8, alignItems: "center", cursor: "pointer" }}>
            <input type="checkbox" checked={level2} onChange={(e) => setLevel2(e.target.checked)} />
            <span>允许调用大模型（项目画像与 AI 分析师）</span>
          </label>
          <div className="form-hint">
            关闭后不会发起任何模型调用：生成项目画像会被拒绝，分析师回退为离线检索式回答。
            <br />
            扫描、索引、资产与能力抽取、跨项目洞察（均为本地规则计算）<b>不受影响</b>。
            <br />
            这是全局总闸——即使项目未标记为敏感、且路由指向云端，关闭后代码也不会离开本机。
          </div>
        </div>
      </div>
      <div className="form-row">
        <div className="form-label">排除模式</div>
        <div className="form-field">
          <textarea
            className="mono"
            rows={5}
            value={patterns}
            onChange={(e) => setPatterns(e.target.value)}
            style={{ width: "100%", resize: "vertical", fontFamily: "var(--font-mono)", fontSize: "var(--fs-sm)" }}
            placeholder={"每行一个，例如：\nnode_modules\ntarget\ndist\n.git"}
          />
          <div className="form-hint">
            凭证文件（<code>.env</code>、私钥等）始终被排除且优先于这里的规则，不可关闭。
          </div>
        </div>
      </div>
      <Button variant="primary" icon="check" onClick={() => void save()}>
        保存扫描设置
      </Button>
    </div>
  );
}

// ══════════════════════════════════════════════════════════════════
// 外观
// ══════════════════════════════════════════════════════════════════

function AppearanceSection({
  appearance,
  onSaved,
  onError,
}: {
  appearance: AppearanceSettings;
  onSaved: (next: AppearanceSettings) => void;
  onError: (msg: string) => void;
}) {
  const [theme, setTheme] = useState(appearance.theme);
  const [reduceMotion, setReduceMotion] = useState(appearance.reduce_motion);

  useEffect(() => {
    setTheme(appearance.theme);
    setReduceMotion(appearance.reduce_motion);
  }, [appearance]);

  const save = useCallback(async () => {
    try {
      const next = await updateAppearance({ theme, reduce_motion: reduceMotion });
      onSaved(next);
    } catch (err) {
      onError(err instanceof Error ? err.message : "保存失败");
    }
  }, [theme, reduceMotion, onSaved, onError]);

  return (
    <Card icon="palette" title="外观" sub="主题与动效偏好。设置存在本机数据库，重启后保持。" style={{ marginBottom: 16 }}>
      <div className="form-row">
        <div className="form-label">主题</div>
        <div className="form-field">
          <div className="chips">
            <button type="button" className={`chip${theme === "dark" ? " is-active" : ""}`} onClick={() => setTheme("dark")}>
              <Icon name="moon" /> 深色
            </button>
            <button type="button" className={`chip${theme === "light" ? " is-active" : ""}`} onClick={() => setTheme("light")}>
              <Icon name="sun" /> 浅色
            </button>
          </div>
        </div>
      </div>
      <div className="form-row">
        <div className="form-label">减弱动效</div>
        <div className="form-field">
          <label style={{ display: "flex", gap: 8, alignItems: "center", cursor: "pointer" }}>
            <input type="checkbox" checked={reduceMotion} onChange={(e) => setReduceMotion(e.target.checked)} />
            <span>关闭过渡与动画（WCAG 2.3.3，效果与系统「减少动态效果」一致）</span>
          </label>
        </div>
      </div>
      <Button variant="primary" icon="check" onClick={() => void save()}>
        保存外观
      </Button>
    </Card>
  );
}

// ══════════════════════════════════════════════════════════════════
// 数据与隐私
// ══════════════════════════════════════════════════════════════════

function DataSection({
  view,
  onCleared,
  onError,
}: {
  view: SettingsView;
  onCleared: () => void;
  onError: (msg: string) => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const [clearing, setClearing] = useState(false);
  const [result, setResult] = useState<ClearResult | null>(null);
  const db = view.db;

  const handleClear = useCallback(async () => {
    setConfirming(false);
    setClearing(true);
    try {
      const r = await clearDerived();
      setResult(r);
      onCleared();
    } catch (err) {
      onError(err instanceof Error ? err.message : "清理失败");
    } finally {
      setClearing(false);
    }
  }, [onCleared, onError]);

  return (
    <Card icon="db" title="数据与隐私" sub="所有数据都存在本机 SQLite，不上传、无账号体系。">
      <KV k="数据库路径" v={<span className="mono" style={{ wordBreak: "break-all" }}>{db.path}</span>} />
      <KV k="占用空间" v={`${db.size_display}（${formatBytes(db.size_bytes)}）`} />
      <KV k="Schema 版本" v={`v${db.schema_version}`} />
      <KV
        k="全文检索"
        v={
          db.fts_available ? (
            <span style={{ color: "var(--color-success)" }}>
              <Icon name="check" /> FTS5 可用
            </span>
          ) : (
            <span style={{ color: "var(--color-warning)" }}>
              <Icon name="alert" /> 不可用，搜索将降级为子串匹配
            </span>
          )
        }
      />

      {db.tables.length > 0 ? (
        <div style={{ marginTop: 12 }}>
          <div className="card-sub" style={{ marginBottom: 6 }}>
            各表行数
          </div>
          <div className="disc-tags">
            {db.tables.map((t) => (
              <Tag key={t.table} mono>
                {t.table}: {t.rows}
              </Tag>
            ))}
          </div>
        </div>
      ) : null}

      {/* 清理结果：展示各表清理行数与刻意保留的数据 */}
      {result !== null ? (
        <div
          style={{
            marginTop: 14,
            padding: "10px 14px",
            borderRadius: "var(--r-md)",
            background: "var(--color-panel-2)",
            border: "1px solid var(--color-border)",
            fontSize: "var(--fs-sm)",
          }}
        >
          <div style={{ marginBottom: 6 }}>
            <Icon name="check" /> 清理完成
          </div>
          <div className="disc-tags" style={{ marginBottom: 8 }}>
            {Object.entries(result.cleared).map(([table, n]) => (
              <Tag key={table} mono>
                {table}: {n} 行
              </Tag>
            ))}
          </div>
          {result.preserved.length > 0 ? (
            <div style={{ color: "var(--color-text-3)" }}>
              保留：{result.preserved.join("、")}
            </div>
          ) : null}
        </div>
      ) : null}

      <AuditAndExport />

      <div style={{ marginTop: 16 }}>
        <Button icon="trash" busy={clearing} onClick={() => setConfirming(true)}>
          清理派生数据
        </Button>
        <div className="form-hint" style={{ marginTop: 6 }}>
          删除资产、能力、关系、洞察、机会与索引；保留设置、项目清单与审计日志。清理后需重新扫描才能恢复。
        </div>
      </div>

      {confirming ? (
        <ConfirmModal
          title="清理派生数据？"
          body={
            <>
              将删除全部<b>派生数据</b>：资产、能力、关系、洞察、机会与全文索引。
              <br />
              <br />
              <b>保留</b>：设置与扫描目录、项目清单（含你的敏感标记与描述）、审计日志。
              <br />
              <br />
              {/* 🔴 反馈标注刻意不列进"保留"：user_feedback 存在 assets/insights 行内，
                  删行必然一起消失。写"保留反馈"是兑现不了的承诺，
                  用户会据此低估操作代价（那是他一条条点出来的北极星指标数据）。
                  与其含糊，不如明确告知会丢——这也与后端 service 层文档一致。 */}
              <b style={{ color: "var(--color-warning)" }}>会一并丢失</b>
              ：你在资产与洞察上的反馈标注（有用/无用/忽略）。
              <br />
              <br />
              清理后这些数据无法恢复，需要重新执行一次完整扫描（含索引与洞察生成）才能重建。
            </>
          }
          confirmLabel="确认清理"
          busy={clearing}
          onConfirm={() => void handleClear()}
          onCancel={() => setConfirming(false)}
        />
      ) : null}
    </Card>
  );
}


/**
 * 单条审计记录。
 *
 * # 🔴 `ok` 是三态，不是布尔
 * - `true`  → 成功的模型调用（绿对勾）
 * - `false` → **失败**的模型调用（红叉 + 展开原因）
 * - `null`  → 本地安全事件，不是一次调用（**不渲染任何成败标记**）
 *
 * 第三态是设计的关键：若把 `null` 当成 `true`，UI 就会在
 * 「用户关闭了敏感项目仅本地约束」旁边显示一个绿色对勾，
 * 把一次**安全降级**说成"操作成功"。那不是审计，是误导。
 *
 * # 为什么失败必须显眼
 * 失败调用同样把 prompt 发出去了——网关是**收到之后**才拒的。
 * 这条记录是"那次数据确实出过网"的唯一凭证，不能被混在一堆成功记录里。
 */
function AuditRow({ a }: { a: AuditView }) {
  // 安全事件没有 model/route，用 db 图标表示"本机"，不显示路由标签
  const isLlmCall = a.ok !== null;
  const failed = a.ok === false;

  return (
    <div className="citation-item">
      <span
        className="kind"
        style={failed ? { color: "var(--color-danger)" } : undefined}
      >
        {/* 🔴 成败图标只在模型调用时出现；安全事件保持中性 */}
        <Icon name={failed ? "x" : a.route.includes("local") ? "db" : "cloud"} />
      </span>
      <div className="label">
        <span style={failed ? { color: "var(--color-danger)" } : undefined}>
          {a.summary}
        </span>
        <div className="supports">
          {a.at}
          {isLlmCall ? ` · ${a.model} · ${a.route_label}` : ""} · {a.job_type}
        </div>
        {failed && a.error ? (
          <div
            className="supports mono"
            style={{ color: "var(--color-danger)", marginTop: 4 }}
            title="请求已被发出，网关返回了错误"
          >
            {/* 错误原因来自 provider，已截断到 180 字符，不含代码原文与 API Key */}
            失败原因：{a.error}
          </div>
        ) : null}
      </div>
    </div>
  );
}

/**
 * 审计日志与配置导出。
 *
 * # 🔴 审计日志是 Local-First 的可信凭证
 * 每次调用模型都留痕（时间/模型/路由/任务/摘要），**失败也留痕**：
 * 请求被网关拒绝（400 未开通/超时/限流）时 prompt 已经出网了，
 * 只记成功会让"我的代码有没有被发到云端"这个问题得到假答案。
 * 用户问起时，这份日志就是唯一能自证的凭证——
 * 没有它，"只用本地模型"只是一句承诺，无法核验。
 *
 * # 🔴 导出是有序键值对，不是对象
 * 后端刻意返回 `Vec<(String,String)>` 保持顺序。这里按数组顺序渲染，
 * 转成对象会丢序（JS 对象的数字键还会被重排）。
 */
function AuditAndExport() {
  const toast = useToast();
  const [showAudit, setShowAudit] = useState(false);
  const { data: audit, loading } = useAsync<AuditView[]>(
    (signal) => getAuditLog({ limit: 30 }, signal),
    [showAudit],
  );

  const handleExport = useCallback(async () => {
    try {
      const entries = await exportSettings();
      // 🔴 生成可下载文本：保持后端给的顺序
      const text = entries.map(([k, v]) => `${k} = ${v}`).join("\n");
      const blob = new Blob([text], { type: "text/plain;charset=utf-8" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = "projectassests-settings.txt";
      a.click();
      URL.revokeObjectURL(url);
      toast.success(`已导出 ${entries.length} 项配置`, "文件：projectassests-settings.txt（不含 API Key 明文）");
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "导出失败");
    }
  }, [toast]);

  return (
    <div style={{ marginTop: 16, paddingTop: 14, borderTop: "1px solid var(--color-border)" }}>
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        <Button icon="doc" onClick={() => setShowAudit((v) => !v)}>
          {showAudit ? "收起审计日志" : "查看审计日志"}
        </Button>
        <Button icon="arr" onClick={() => void handleExport()}>
          导出配置
        </Button>
      </div>

      {showAudit ? (
        <div style={{ marginTop: 12 }}>
          {loading && audit === null ? (
            <Loading rows={2} label="加载审计日志" />
          ) : audit === null || audit.length === 0 ? (
            <InlineEmpty>
              还没有模型调用记录。每次调用大模型（画像、分析师）都会在这里留痕：
              时间、模型、路由（本地/云端）、任务类型。
              失败的调用同样会记录——请求被拒绝时数据已经发出，这里是你唯一的凭证。
            </InlineEmpty>
          ) : (
            <div className="citation-list">
              {audit.map((a, i) => (
                <AuditRow key={i} a={a} />
              ))}
            </div>
          )}
        </div>
      ) : null}
    </div>
  );
}
