import { useEffect, useState, type ReactNode } from "react";

export function cn(...parts: Array<string | false | null | undefined>) {
  return parts.filter(Boolean).join(" ");
}

export function Btn({
  children,
  variant = "ghost",
  onClick,
  className,
  title,
  disabled = false,
}: {
  children: ReactNode;
  variant?: "primary" | "accent" | "secondary" | "ghost" | "danger";
  onClick?: () => void;
  className?: string;
  title?: string;
  disabled?: boolean;
}) {
  const styles = {
    primary:
      "bg-accent text-white font-semibold hover:bg-accent-strong active:bg-[#102f85] border border-accent-strong shadow-[0_1px_0_rgba(255,255,255,0.18)_inset]",
    accent:
      "border border-accent/45 text-accent bg-accent-soft hover:bg-[#d6defb] hover:border-accent",
    secondary:
      "border border-line-2 text-text bg-panel hover:border-accent-dim hover:text-accent",
    ghost: "border border-transparent text-muted hover:text-text hover:bg-panel-2",
    danger:
      "border border-danger/40 text-danger bg-panel hover:bg-danger-soft hover:border-danger",
  }[variant];

  return (
    <button
      type="button"
      title={title}
      onClick={onClick}
      disabled={disabled}
      className={cn(
        "h-8 px-3 rounded-[3px] text-[12px] tracking-wide transition-colors whitespace-nowrap",
        styles,
        disabled ? "opacity-45 cursor-not-allowed hover:bg-inherit" : "",
        className,
      )}
    >
      {children}
    </button>
  );
}

export function Field({
  label,
  children,
  width,
  hint,
}: {
  label: string;
  children: ReactNode;
  width?: string;
  hint?: string;
}) {
  return (
    <label className="flex shrink-0 flex-col gap-1" style={{ width }}>
      <span className="whitespace-nowrap text-[12px]  text-dim font-mono">
        {label}
      </span>
      {children}
      {hint ? (
        <span className="whitespace-nowrap text-[12px] text-dim">{hint}</span>
      ) : null}
    </label>
  );
}

export function Num({
  value,
  unit,
  suffix,
  width = "86px",
  onChange,
  min,
  max,
}: {
  value: string | number;
  unit?: string;
  suffix?: string;
  width?: string;
  /** 只在输入是合法数字时回调，非法中间态留在输入框里。 */
  onChange?: (value: number) => void;
  min?: number;
  max?: number;
}) {
  const [draft, setDraft] = useState(String(value));

  // 设置从磁盘回填后同步到输入框。
  useEffect(() => {
    setDraft(String(value));
  }, [value]);

  const commit = (next: string) => {
    setDraft(next);
    if (next.trim() === "") return;
    const parsed = Number(next);
    if (!Number.isFinite(parsed)) return;
    if (min !== undefined && parsed < min) return;
    if (max !== undefined && parsed > max) return;
    onChange?.(parsed);
  };

  return (
    <div
      className="h-8 flex items-center justify-between gap-1 border border-line bg-panel px-2 rounded-[3px] focus-within:border-accent-dim"
      style={{ width }}
    >
      <input
        value={draft}
        inputMode="numeric"
        onChange={(e) => commit(e.target.value)}
        className="num w-full bg-transparent text-[12px] outline-none text-text"
      />
      {unit ? (
        <span className="num text-[12px] text-dim shrink-0">{unit}</span>
      ) : null}
      {suffix ? (
        <span className="text-[12px] text-dim shrink-0">{suffix}</span>
      ) : null}
    </div>
  );
}

export function Dot({ tone }: { tone: "ok" | "warn" | "err" | "idle" }) {
  const color = {
    ok: "bg-accent",
    warn: "bg-warn",
    err: "bg-danger",
    idle: "bg-dim",
  }[tone];
  return <span className={cn("inline-block size-[6px] rounded-full", color)} />;
}

export function Divider() {
  return <span className="h-5 w-px bg-line shrink-0" />;
}
