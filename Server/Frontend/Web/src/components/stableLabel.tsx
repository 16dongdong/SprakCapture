/**
 * 定义状态控件的稳定文本槽位。
 *
 * 运行上下文：工具栏中的状态文案会随异步操作切换；候选文案作为不可访问的测量层参与布局，当前文案覆盖显示层。
 * 参数：value 为当前可见文案，candidates 为同一控件可能出现的本地化文案集合。
 * 失败语义：候选集合为空时仍按当前 value 渲染，不引入额外的运行时失败分支。
 */
interface StableLabelProps {
  value: string;
  candidates: readonly string[];
}

/**
 * 为状态切换文案保留最大固有宽度，避免异步操作期间推动相邻控件。
 *
 * 运行上下文：测量层只用 data-text + CSS content 占宽，不把候选字符串写入 DOM 文本节点，
 * 这样 button 的 textContent / 可访问名不会拼成「开始录制暂停录制…」。
 * 参数：props 提供当前值和完整候选集合。
 * 失败语义：重复候选值会在渲染前去重，不影响当前文案和布局宽度。
 */
export function StableLabel({
  value,
  candidates,
}: StableLabelProps) {
  const uniqueCandidates = [...new Set(candidates)];

  return (
    <span className="stableLabel">
      <span aria-hidden="true" className="stableLabelMeasure">
        {uniqueCandidates.map((candidate) => (
          <span key={candidate} data-text={candidate} />
        ))}
      </span>
      <span className="stableLabelValue">{value}</span>
    </span>
  );
}
