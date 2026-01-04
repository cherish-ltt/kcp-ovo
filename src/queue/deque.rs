//! KCP双向队列实现
//!
//! 本模块提供了基于Rust标准库VecDeque的KCP双向队列封装

use std::collections::VecDeque;
use super::segment::Segment;

/// KCP双向队列
///
/// 对应C代码中的IQUEUEHEAD，使用Rust标准库的VecDeque实现
/// 提供类型安全的队列操作，用于管理KCP的各种数据队列
#[derive(Debug, Clone)]
pub struct KcpDeque {
    /// 内部使用VecDeque存储
    inner: VecDeque<Segment>,
}

impl KcpDeque {
    /// 创建新的空队列
    ///
    /// # 返回
    ///
    /// 返回一个不包含任何元素的KcpDeque实例
    ///
    /// # 示例
    ///
    /// ```ignore
    /// use kcp_ovo::queue::deque::KcpDeque;
    ///
    /// let deque = KcpDeque::new();
    /// assert!(deque.is_empty());
    /// ```
    pub fn new() -> Self {
        Self {
            inner: VecDeque::new(),
        }
    }

    /// 在队尾添加元素
    ///
    /// # 参数
    ///
    /// - `seg`: 要添加的数据段
    ///
    /// # 示例
    ///
    /// ```ignore
    /// use kcp_ovo::queue::{deque::KcpDeque, segment::Segment};
    ///
    /// let mut deque = KcpDeque::new();
    /// let seg = Segment::new(vec![1, 2, 3]);
    /// deque.push_back(seg);
    /// assert_eq!(deque.len(), 1);
    /// ```
    pub fn push_back(&mut self, seg: Segment) {
        self.inner.push_back(seg);
    }

    /// 在队头添加元素
    ///
    /// # 参数
    ///
    /// - `seg`: 要添加的数据段
    pub fn push_front(&mut self, seg: Segment) {
        self.inner.push_front(seg);
    }

    /// 从队头移除元素
    ///
    /// # 返回
    ///
    /// 如果队列不为空，返回队头元素；否则返回None
    ///
    /// # 示例
    ///
    /// ```ignore
    /// use kcp_ovo::queue::{deque::KcpDeque, segment::Segment};
    ///
    /// let mut deque = KcpDeque::new();
    /// let seg = Segment::new(vec![1, 2, 3]);
    /// deque.push_back(seg.clone());
    /// let removed = deque.pop_front();
    /// assert!(removed.is_some());
    /// assert!(deque.is_empty());
    /// ```
    pub fn pop_front(&mut self) -> Option<Segment> {
        self.inner.pop_front()
    }

    /// 从队尾移除元素
    ///
    /// # 返回
    ///
    /// 如果队列不为空，返回队尾元素；否则返回None
    pub fn pop_back(&mut self) -> Option<Segment> {
        self.inner.pop_back()
    }

    /// 获取队头元素的引用
    ///
    /// # 返回
    ///
    /// 如果队列不为空，返回队头元素的引用；否则返回None
    pub fn front(&self) -> Option<&Segment> {
        self.inner.front()
    }

    /// 获取队头元素的可变引用
    pub fn front_mut(&mut self) -> Option<&mut Segment> {
        self.inner.front_mut()
    }

    /// 获取队尾元素的引用
    ///
    /// # 返回
    ///
    /// 如果队列不为空，返回队尾元素的引用；否则返回None
    pub fn back(&self) -> Option<&Segment> {
        self.inner.back()
    }

    /// 获取队尾元素的可变引用
    pub fn back_mut(&mut self) -> Option<&mut Segment> {
        self.inner.back_mut()
    }

    /// 检查队列是否为空
    ///
    /// # 返回
    ///
    /// 如果队列不包含任何元素，返回true；否则返回false
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// 获取队列长度
    ///
    /// # 返回
    ///
    /// 返回队列中元素的数量
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// 清空队列
    ///
    /// 移除队列中的所有元素
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// 获取迭代器
    pub fn iter(&self) -> std::collections::vec_deque::Iter<'_, Segment> {
        self.inner.iter()
    }

    /// 获取可变迭代器
    pub fn iter_mut(&mut self) -> std::collections::vec_deque::IterMut<'_, Segment> {
        self.inner.iter_mut()
    }
}

impl Default for KcpDeque {
    fn default() -> Self {
        Self::new()
    }
}

impl IntoIterator for KcpDeque {
    type Item = Segment;
    type IntoIter = std::collections::vec_deque::IntoIter<Segment>;

    fn into_iter(self) -> Self::IntoIter {
        self.inner.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::segment::Segment;

    #[test]
    fn test_deque_new() {
        let deque = KcpDeque::new();
        assert!(deque.is_empty());
        assert_eq!(deque.len(), 0);
    }

    #[test]
    fn test_push_back_pop_front() {
        let mut deque = KcpDeque::new();
        let seg1 = Segment::new(vec![1, 2, 3]);
        let seg2 = Segment::new(vec![4, 5, 6]);

        deque.push_back(seg1);
        deque.push_back(seg2);

        assert_eq!(deque.len(), 2);

        let removed = deque.pop_front();
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().data, vec![1, 2, 3]);
        assert_eq!(deque.len(), 1);
    }

    #[test]
    fn test_push_front_pop_back() {
        let mut deque = KcpDeque::new();
        let seg = Segment::new(vec![1, 2, 3]);

        deque.push_front(seg.clone());
        let removed = deque.pop_back();

        assert!(removed.is_some());
        assert_eq!(removed.unwrap().data, vec![1, 2, 3]);
    }

    #[test]
    fn test_front_back() {
        let mut deque = KcpDeque::new();
        let seg1 = Segment::new(vec![1, 2, 3]);
        let seg2 = Segment::new(vec![4, 5, 6]);

        deque.push_back(seg1);
        deque.push_back(seg2);

        let front = deque.front();
        assert!(front.is_some());
        assert_eq!(front.unwrap().data, vec![1, 2, 3]);

        let back = deque.back();
        assert!(back.is_some());
        assert_eq!(back.unwrap().data, vec![4, 5, 6]);
    }

    #[test]
    fn test_clear() {
        let mut deque = KcpDeque::new();
        deque.push_back(Segment::new(vec![1, 2, 3]));
        deque.push_back(Segment::new(vec![4, 5, 6]));

        assert_eq!(deque.len(), 2);
        deque.clear();
        assert!(deque.is_empty());
    }

    #[test]
    fn test_iter() {
        let mut deque = KcpDeque::new();
        deque.push_back(Segment::new(vec![1]));
        deque.push_back(Segment::new(vec![2]));
        deque.push_back(Segment::new(vec![3]));

        let mut count = 0;
        for seg in deque.iter() {
            assert_eq!(seg.data.len(), 1);
            count += 1;
        }
        assert_eq!(count, 3);
    }
}
