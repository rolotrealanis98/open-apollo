'use client'

import { Fragment } from 'react'
import { Highlight } from 'prism-react-renderer'

export function Fence({
  children,
  language,
}: {
  children: React.ReactNode
  language: string
}) {
  const code = typeof children === 'string' ? children.trimEnd() : String(children ?? '')
  return (
    <Highlight
      code={code}
      language={language ?? 'text'}
      theme={{ plain: {}, styles: [] }}
    >
      {({ className, style, tokens, getTokenProps }) => (
        <pre className={className} style={style}>
          <code>
            {tokens.map((line, lineIndex) => (
              <Fragment key={lineIndex}>
                {line
                  .filter((token) => !token.empty)
                  .map((token, tokenIndex) => (
                    <span key={tokenIndex} {...getTokenProps({ token })} />
                  ))}
                {'\n'}
              </Fragment>
            ))}
          </code>
        </pre>
      )}
    </Highlight>
  )
}
