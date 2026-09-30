let g:termleaf_follow = get(g:, 'termleaf_follow', 1)
let s:timer = -1
let s:last = ''
function! s:Dir() abort
  if !empty($XDG_RUNTIME_DIR)
    return $XDG_RUNTIME_DIR . '/termleaf'
  endif
  return (empty($TMPDIR) ? '/tmp' : $TMPDIR) . '/termleaf-' . $USER
endfunction
function! s:Send(...) abort
  let s:timer = -1
  if !g:termleaf_follow
    return
  endif
  let l:msg = 'follow ' . line('.') . ' ' . col('.') . ' ' . expand('%:p')
  if l:msg ==# s:last
    return
  endif
  let s:last = l:msg
  for l:sock in glob(s:Dir() . '/*.sock', 1, 1)
    silent! let l:ch = ch_open('unix:' . l:sock, {'mode': 'raw'})
    if ch_status(l:ch) ==# 'open'
      call ch_sendraw(l:ch, l:msg . "\n")
      call ch_close(l:ch)
    endif
  endfor
endfunction
function! s:Schedule() abort
  if s:timer != -1
    call timer_stop(s:timer)
  endif
  let s:timer = timer_start(150, function('s:Send'))
endfunction
augroup termleaf_follow
  autocmd!
  autocmd CursorMoved,CursorMovedI,BufEnter *.tex call s:Schedule()
augroup END
command! TermleafFollowToggle let g:termleaf_follow = !g:termleaf_follow | echo 'termleaf follow ' . (g:termleaf_follow ? 'on' : 'off')
