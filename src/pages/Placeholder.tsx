interface PlaceholderProps {
  title: string;
  milestone: string;
  summary: string;
  items: string[];
}

/** 未开工模块的占位页。写清楚"哪个里程碑做、做什么"，比留一片空白有用。 */
export default function Placeholder({ title, milestone, summary, items }: PlaceholderProps) {
  return (
    <div className="placeholder">
      <div className="placeholder__badge">{milestone}</div>
      <h2 className="placeholder__title">{title}</h2>
      <p className="placeholder__summary">{summary}</p>
      <ul className="placeholder__list">
        {items.map((item) => (
          <li key={item}>{item}</li>
        ))}
      </ul>
    </div>
  );
}
