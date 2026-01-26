//  Copyright 2020 Two Sigma Investments, LP.
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.


/// `impl_ord_by!` provides ordering on a type given a closure.
/// We use it for providing ordering to types that are used in a BinaryHeap.
#[macro_export]
macro_rules! impl_ord_by {
    ($type:ident$(<$($gen:tt),+>)?, $cmp_fn:expr) => {
        impl$(<$($gen),+>)? Ord for $type$(<$($gen),+>)? {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                $cmp_fn(self, other)
            }
        }

        impl$(<$($gen),+>)? PartialOrd for $type$(<$($gen),+>)? {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }

        impl$(<$($gen),+>)? PartialEq for $type$(<$($gen),+>)? {
            fn eq(&self, other: &Self) -> bool {
                self.cmp(other) == std::cmp::Ordering::Equal
            }
        }

        impl$(<$($gen),+>)? Eq for $type$(<$($gen),+>)? {}
    };
}
