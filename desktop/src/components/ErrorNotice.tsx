export function ErrorNotice({
  title,
  detail,
  retry,
  dismiss,
}: {
  title: string;
  detail: string;
  retry?: () => void;
  dismiss?: () => void;
}) {
  return (
    <div className="error-notice" role="alert">
      <div className="error-heading">
        <strong>{title}</strong>
        {dismiss && (
          <button
            className="text-button"
            onClick={dismiss}
            aria-label="关闭错误提示"
          >
            关闭
          </button>
        )}
      </div>
      <details className="error-details">
        <summary>查看错误详情</summary>
        <pre>{detail}</pre>
      </details>
      {retry && <button onClick={retry}>重试</button>}
    </div>
  );
}
