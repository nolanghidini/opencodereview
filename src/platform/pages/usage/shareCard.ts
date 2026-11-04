import faviconUrl from "../../../../assets/favicon.svg?url";
import type { UsageDay, UsageRange, UsageReport } from "../../../types";
import { formatFlowDuration } from "../../lib/reviewFlow";
import { attributedRows, compactTokens, dayAxisLabel, pct } from "./format";

/**
 * Everything the share image shows. It is built only from aggregate numbers, so no repository,
 * pull request, account or model name can reach an image people post publicly.
 */
interface ShareCardData {
  days: UsageRange;
  tokens: string;
  cacheRead: string | null;
  stats: { label: string; value: string }[];
  providers: string[];
  bars: { day: string; tokens: number }[];
}

const PROVIDER_NAMES: Record<string, string> = {
  claude: "Claude",
  codex: "Codex",
  opencode: "OpenCode",
};

export function shareCardData(report: UsageReport): ShareCardData {
  const { totals } = report;
  const plural = (count: number, word: string) => `${word}${count === 1 ? "" : "s"}`;
  const providers = attributedRows(report.byProvider);
  return {
    days: report.days,
    tokens: compactTokens(totals.tokens),
    cacheRead: totals.cacheReadTokens > 0 ? compactTokens(totals.cacheReadTokens) : null,
    stats: [
      { label: plural(totals.runs, "Review"), value: totals.runs.toLocaleString("en-US") },
      { label: plural(totals.prs, "Pull request"), value: totals.prs.toLocaleString("en-US") },
      { label: plural(totals.findingsFound, "Finding"), value: totals.findingsFound.toLocaleString("en-US") },
      { label: "Median review", value: formatFlowDuration(totals.medianDurationMs) },
    ],
    providers:
      providers.length >= 2
        ? providers.map((row) => `${PROVIDER_NAMES[row.provider ?? ""] ?? row.label} ${pct(row.tokens, totals.tokens)}`)
        : [],
    bars: report.byDay.map((day: UsageDay) => ({ day: day.day, tokens: day.tokens })),
  };
}

export const SHARE_WIDTH = 1200;
export const SHARE_HEIGHT = 630;
const PAD = 72;

function token(name: string) {
  return getComputedStyle(document.documentElement).getPropertyValue(`--color-${name}`).trim();
}

function loadImage(src: string) {
  return new Promise<HTMLImageElement>((resolve, reject) => {
    const image = new Image();
    image.onload = () => resolve(image);
    image.onerror = () => reject(new Error("Could not load the OpenCodeReview mark"));
    image.src = src;
  });
}

function roundedTop(ctx: CanvasRenderingContext2D, x: number, y: number, width: number, height: number) {
  const radius = Math.min(4, width / 2, height);
  ctx.beginPath();
  ctx.moveTo(x, y + height);
  ctx.lineTo(x, y + radius);
  ctx.arcTo(x, y, x + radius, y, radius);
  ctx.arcTo(x + width, y, x + width, y + radius, radius);
  ctx.lineTo(x + width, y + height);
  ctx.closePath();
  ctx.fill();
}

/** Draws the card at 2x into `canvas` and returns it as a PNG. */
export async function renderShareCard(canvas: HTMLCanvasElement, data: ShareCardData): Promise<Blob> {
  const scale = 2;
  canvas.width = SHARE_WIDTH * scale;
  canvas.height = SHARE_HEIGHT * scale;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("This browser cannot draw the share image");
  await document.fonts.ready;
  const mark = await loadImage(faviconUrl);
  ctx.scale(scale, scale);

  const sans = getComputedStyle(document.documentElement).getPropertyValue("--font-sans").trim();
  const mono = getComputedStyle(document.documentElement).getPropertyValue("--font-mono").trim();
  const [canvasColor, fg, muted, subtle, line, brand, brandSoft] = [
    "canvas",
    "fg",
    "fg-muted",
    "fg-subtle",
    "line",
    "brand",
    "brand-soft",
  ].map(token);

  ctx.fillStyle = canvasColor;
  ctx.fillRect(0, 0, SHARE_WIDTH, SHARE_HEIGHT);
  const glow = ctx.createRadialGradient(SHARE_WIDTH - 160, 40, 0, SHARE_WIDTH - 160, 40, 620);
  glow.addColorStop(0, brandSoft);
  glow.addColorStop(1, "transparent");
  ctx.fillStyle = glow;
  ctx.fillRect(0, 0, SHARE_WIDTH, SHARE_HEIGHT);

  // Header: mark, name and period.
  ctx.drawImage(mark, PAD, PAD - 6, 40, 40);
  ctx.textBaseline = "middle";
  ctx.fillStyle = fg;
  ctx.font = `500 26px ${sans}`;
  ctx.fillText("OpenCodeReview", PAD + 54, PAD + 14);
  ctx.textAlign = "right";
  ctx.fillStyle = subtle;
  ctx.font = `400 22px ${sans}`;
  ctx.fillText(`Last ${data.days} days`, SHARE_WIDTH - PAD, PAD + 14);
  ctx.textAlign = "left";

  // Hero number.
  ctx.textBaseline = "alphabetic";
  ctx.fillStyle = fg;
  ctx.font = `500 128px ${sans}`;
  ctx.fillText(data.tokens, PAD - 4, 262);
  ctx.fillStyle = muted;
  ctx.font = `400 26px ${sans}`;
  ctx.fillText("tokens of AI code review", PAD, 308);
  if (data.cacheRead) {
    ctx.fillStyle = subtle;
    ctx.font = `400 20px ${sans}`;
    ctx.fillText(`+ ${data.cacheRead} read from cache`, PAD, 342);
  }

  // Daily bars on the right.
  const chart = { left: 620, right: SHARE_WIDTH - PAD, top: 150, bottom: 330 };
  const peak = Math.max(1, ...data.bars.map((bar) => bar.tokens));
  const slot = (chart.right - chart.left) / Math.max(1, data.bars.length);
  const width = Math.max(2, Math.min(36, slot * 0.62));
  ctx.strokeStyle = line;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(chart.left, chart.bottom + 0.5);
  ctx.lineTo(chart.right, chart.bottom + 0.5);
  ctx.stroke();
  ctx.fillStyle = brand;
  data.bars.forEach((bar, index) => {
    if (bar.tokens <= 0) return;
    const height = Math.max(3, (bar.tokens / peak) * (chart.bottom - chart.top));
    roundedTop(ctx, chart.left + index * slot + (slot - width) / 2, chart.bottom - height, width, height);
  });
  if (data.bars.length) {
    ctx.fillStyle = subtle;
    ctx.font = `400 15px ${mono}`;
    ctx.fillText(dayAxisLabel(data.bars[0].day), chart.left, chart.bottom + 28);
    ctx.textAlign = "right";
    ctx.fillText(dayAxisLabel(data.bars[data.bars.length - 1].day), chart.right, chart.bottom + 28);
    ctx.textAlign = "left";
  }

  // Stats row.
  ctx.strokeStyle = line;
  ctx.beginPath();
  ctx.moveTo(PAD, 400.5);
  ctx.lineTo(SHARE_WIDTH - PAD, 400.5);
  ctx.stroke();
  const column = (SHARE_WIDTH - PAD * 2) / data.stats.length;
  data.stats.forEach((stat, index) => {
    const x = PAD + index * column;
    ctx.fillStyle = fg;
    ctx.font = `500 44px ${sans}`;
    ctx.fillText(stat.value, x, 474);
    ctx.fillStyle = subtle;
    ctx.font = `400 20px ${sans}`;
    ctx.fillText(stat.label, x, 506);
  });

  // Footer.
  ctx.textBaseline = "middle";
  ctx.fillStyle = muted;
  ctx.font = `400 19px ${sans}`;
  if (data.providers.length) ctx.fillText(data.providers.join("  ·  "), PAD, SHARE_HEIGHT - PAD + 14);
  ctx.textAlign = "right";
  ctx.fillStyle = subtle;
  ctx.fillText("opencodereview.run", SHARE_WIDTH - PAD, SHARE_HEIGHT - PAD + 14);

  return new Promise((resolve, reject) =>
    canvas.toBlob((blob) => (blob ? resolve(blob) : reject(new Error("Could not create the image"))), "image/png"),
  );
}
