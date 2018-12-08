set title "Pull Request CI duration for rust-lang/rust\n95th percentile\nWeekly intervals"
set ylabel "Duration (hours)"
set xlabel "Date PR Opened"
set xdata time
set timefmt "%Y-%m-%d"
set format x "%m/%d/%y"
set datafile separator ","
set grid
set samples 1000
datafile = 'merge_time.csv'
# set yrange [0:]
plot \
    datafile using 1:($2/60) smooth mcsplines title "Overall", \
    datafile using 1:($3/60) smooth mcsplines title "AppVeyor", \
    datafile using 1:($4/60) smooth mcsplines title "Travis", \
